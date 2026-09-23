// 真实取数通道：把声明式数据集接到 sqlx 驱动层
//
// 三条不变量（改这个文件前先读）：
//   1. 凭据只来自调用方传入的 DBConfig。落盘的 connections 自 T-007 起密码恒为空，
//      所以这里绝不能"顺手"回读 load_connections() 来补密码——那会把明文重新写回磁盘。
//   2. 打到数据库的 SQL 只有一种恒定形状：source_sql 生成的单表 SELECT。
//      join / 过滤 / 计算列 / 聚合全在内存里跑，所以跨库与方言无关。
//   3. 每条 SQL 在执行前重新过一遍词法分类器做只读兜底，即使上游标识符白名单
//      将来被放宽，也不会把注入直接漏到执行层。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;

use crate::db::sql_classify::{classify, SafetyClass};
use crate::{dispatch_db_type, run_dispatch, DBConfig};

use super::dataset::{
    self, assert_ident, source_sql, DatasetSpec, RowSource, SchemaCache, SourceRef,
};
use super::view::{self, ViewSpec};
use super::expr::Value;
use super::table::Table;

/// connection_id → 连接配置。一个数据集可以同时引用多个连接，这就是跨库。
#[derive(Clone, Default)]
pub struct DbSource {
    configs: HashMap<String, DBConfig>,
}

impl DbSource {
    pub fn new(configs: &[DBConfig]) -> Self {
        DbSource {
            configs: configs
                .iter()
                .map(|c| (c.id.clone(), c.clone()))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    pub fn connection_ids(&self) -> Vec<String> {
        self.configs.keys().cloned().collect()
    }

    /// 定位连接，并核对"spec 声明的方言 == 该连接真实方言"。
    /// 方言写错会让 quote_ident 用错引号（MySQL 反引号打到 SQLite 上必然语法错），
    /// 与其等数据库报一个看不懂的错，不如在计划期就指名道姓地拒掉。
    pub fn resolve(&self, src: &SourceRef) -> Result<(DBConfig, &'static str), String> {
        let cfg = self.configs.get(&src.connection_id).ok_or_else(|| {
            format!(
                "源 {} 引用的连接 {} 不在本会话已解锁的连接里，请先在连接面板建立连接",
                src.alias, src.connection_id
            )
        })?;
        let real = dispatch_db_type(cfg)?;
        if !src.database_type.trim().is_empty() && real != src.database_type {
            return Err(format!(
                "源 {} 声明方言 {}，但连接 {} 实际是 {}，请修正数据集而不是连接",
                src.alias, src.database_type, cfg.name, real
            ));
        }
        Ok((cfg.clone(), real))
    }
}

/// 没写方言的 source 按连接真实方言回填。写错方言仍然报错——那通常是把 MySQL
/// 的反引号写法带进了 SQLite 连接，静默改掉反而会让人查不出列为什么不存在。
pub fn aligned(src: &SourceRef, real: &str) -> SourceRef {
    if src.database_type.trim().is_empty() {
        SourceRef { database_type: real.to_string(), ..src.clone() }
    } else {
        src.clone()
    }
}

/// 命令层第一步：把省略了 database_type 的 source 按连接真实方言回填。
/// 方言本来就是连接的属性，不该要求每个 spec（尤其是 AI 草稿）重抄一遍；
/// 回填一次之后 plan / execute / previews 看到的是同一份方言，不会各猜各的。
pub fn align_dialects(spec: &DatasetSpec, source: &DbSource) -> Result<DatasetSpec, String> {
    let mut sources = spec.sources.clone();
    for src in sources.iter_mut() {
        if src.database_type.trim().is_empty() {
            let (_, real) = source.resolve(src)?;
            src.database_type = real.to_string();
        }
    }
    Ok(DatasetSpec { sources, ..spec.clone() })
}

impl RowSource for DbSource {
    fn fetch<'a>(
        &'a self,
        src: &'a SourceRef,
        cap: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Table, String>> + Send + 'a>> {
        Box::pin(async move {
            let (cfg, real) = self.resolve(src)?;
            let sql = guarded_sql(&aligned(src, real), cap)?;
            let (meta, rows, _, _) = run_dispatch(&cfg, &sql).await?;
            let columns: Vec<String> = meta.iter().map(|m| m.name.clone()).collect();
            Ok(dataset::table_from_tagged(&columns, &rows))
        })
    }
}

/// Arc<DbSource> 也要能当数据源用（命令层持有所有权）
impl RowSource for Arc<DbSource> {
    fn fetch<'a>(
        &'a self,
        src: &'a SourceRef,
        cap: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Table, String>> + Send + 'a>> {
        (**self).fetch(src, cap)
    }
}

/// 生成源 SQL 并再过一次只读分类器
pub fn guarded_sql(src: &SourceRef, cap: usize) -> Result<String, String> {
    let sql = source_sql(src, cap)?;
    let c = classify(&sql);
    if c.safety != SafetyClass::ReadOnlySafe {
        return Err(format!(
            "源 {} 生成的 SQL 未通过只读分类：kind={:?} safety={:?}，SQL={}",
            src.alias, c.kind, c.safety, sql
        ));
    }
    Ok(sql)
}

// ==================== 列清单探查 ====================

/// 字符串字面量内联。
///
/// run_dispatch 只接受 SQL 文本、不接绑定参数，而 information_schema 探查必须把
/// 库名/表名放进 WHERE 的字面量里，所以这里自己保证不可逃逸：
/// 反斜杠、控制字符、NUL 直接拒绝，单引号翻倍。MySQL 默认把 `\` 当转义符，
/// 只翻倍 `'` 在 MySQL 上是不够的（`\'` 能把收尾引号吃掉）。
pub fn text_lit(raw: &str) -> Result<String, String> {
    if raw.contains('\\') {
        return Err(format!("标识符含反斜杠，拒绝内联为字面量: {}", raw));
    }
    if raw.chars().any(|c| c.is_control()) {
        return Err("标识符含控制字符，拒绝内联为字面量".to_string());
    }
    Ok(format!("'{}'", raw.replace('\'', "''")))
}

/// 一列的结构信息。名字给本机校验用，类型只给模型看。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    /// 数据库自报的类型，如 `decimal(12,2)`、`INTEGER`。取不到就是空串
    #[serde(default)]
    pub data_type: String,
}

/// 探查列清单的 SQL。三种方言都保证"第一列列名、第二列类型"，便于上层统一取值。
pub fn describe_sql(dialect: &str, schema: &str, table: &str, database: &str) -> Result<String, String> {
    let tbl = assert_ident(table, "表")?;
    match dialect {
        "mysql" => {
            let db = if schema.trim().is_empty() { database } else { schema };
            if db.trim().is_empty() {
                return Err("MySQL 探查列需要库名（连接配置里的数据库名）".to_string());
            }
            // COLUMN_TYPE 比 DATA_TYPE 带长度/精度：decimal(12,2) 才能让模型知道要不要 CAST
            Ok(format!(
                "SELECT COLUMN_NAME, COLUMN_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION",
                text_lit(db)?,
                text_lit(&tbl)?
            ))
        }
        "postgresql" => {
            let sch = if schema.trim().is_empty() { "public" } else { schema.trim() };
            Ok(format!(
                "SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = {} AND table_name = {} ORDER BY ordinal_position",
                text_lit(sch)?,
                text_lit(&tbl)?
            ))
        }
        // SQLite 一个连接就是一个文件，没有 schema 层；pragma_table_info 自 3.16 起可用
        "sqlite" => Ok(format!(
            "SELECT name, type FROM pragma_table_info({}) ORDER BY cid",
            text_lit(&tbl)?
        )),
        other => Err(format!("不支持的方言: {}", other)),
    }
}

/// 真实探查：拿到表的列名 + 类型清单。
/// 这是"AI 幻觉列名"能被抓出来的前提——提示词里给的是数据库真实列，
/// 校验时也比对的是数据库真实列，而不是 AI 自己编的那份。
pub async fn describe_columns_typed(
    cfg: &DBConfig,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>, String> {
    let dialect = dispatch_db_type(cfg)?;
    let sql = describe_sql(dialect, schema, table, &cfg.database)?;
    let c = classify(&sql);
    if c.safety != SafetyClass::ReadOnlySafe {
        return Err(format!("探查 SQL 未通过只读分类：{:?}", c.safety));
    }
    let (_, rows, _, _) = run_dispatch(cfg, &sql).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let v = dataset::tagged_to_value(r.first().unwrap_or(&serde_json::Value::Null));
        let Some(name) = v.as_text() else {
            return Err(format!("探查 {} 返回了非文本列名", table));
        };
        // 类型缺失不能整体失败：有的库（或建表时没写类型的 sqlite 列）就是给不出
        let data_type = r
            .get(1)
            .map(dataset::tagged_to_value)
            .and_then(|t| t.as_text())
            .unwrap_or_default();
        out.push(ColumnInfo {
            name,
            data_type: data_type.trim().to_string(),
        });
    }
    if out.is_empty() {
        return Err(format!(
            "{} 没有可读取的列：表不存在、没有权限，或者连接指向了别的库",
            table
        ));
    }
    Ok(out)
}

/// 只要列名：本机校验比对的是名字，类型对校验没有意义。
pub async fn describe_columns(
    cfg: &DBConfig,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, String> {
    Ok(describe_columns_typed(cfg, schema, table)
        .await?
        .into_iter()
        .map(|c| c.name)
        .collect())
}

/// 组装 schema 缓存：spec 自带的列清单优先，其次沿用调用方已给的缓存，
/// 最后才去数据库探查。探查按 (connection_id, schema, table) 去重，
/// 同一个表被 join 两次也只问一次。
pub async fn build_schemas(
    spec: &DatasetSpec,
    source: &DbSource,
    provided: Option<&SchemaCache>,
) -> Result<SchemaCache, String> {
    let mut out: SchemaCache = HashMap::new();
    let mut probed: HashMap<(String, String, String), Vec<String>> = HashMap::new();
    for src in &spec.sources {
        if !src.columns.is_empty() {
            out.insert(src.alias.clone(), src.columns.clone());
            continue;
        }
        if let Some(cols) = provided.and_then(|p| p.get(&src.alias)) {
            if !cols.is_empty() {
                out.insert(src.alias.clone(), cols.clone());
                continue;
            }
        }
        let key = (
            src.connection_id.clone(),
            src.schema.clone(),
            src.table.clone(),
        );
        let cols = match probed.get(&key) {
            Some(c) => c.clone(),
            None => {
                let (cfg, _) = source.resolve(src)?;
                let c = describe_columns(&cfg, &src.schema, &src.table).await?;
                probed.insert(key, c.clone());
                c
            }
        };
        out.insert(src.alias.clone(), cols);
    }
    Ok(out)
}

// ==================== 命令层返回体 ====================

#[derive(Debug, Clone, Serialize)]
pub struct SqlPreview {
    /// 属于哪个数据集（一张报表可以有多个数据集）
    pub dataset: String,
    pub alias: String,
    pub connection_id: String,
    pub connection_name: String,
    pub database_type: String,
    pub sql: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    /// 人类可读的算子链，前端执行计划面板与 AI 回显共用
    pub steps: Vec<String>,
    pub sqls: Vec<SqlPreview>,
    pub schemas: SchemaCache,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceRowStat {
    pub alias: String,
    pub rows: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DatasetPayload {
    pub columns: Vec<String>,
    /// 与 columns 对齐的行，避免前端用 Object.keys(首行) 猜列
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub source_rows: Vec<SourceRowStat>,
    /// 撞上 max_rows 硬闸的源；UI 必须显式提示"结果不完整"
    pub truncated: Vec<String>,
    pub partial: bool,
    pub generated_sql: Vec<SqlPreview>,
    pub steps: Vec<String>,
    pub elapsed_ms: u64,
}

/// 生成"查看生成的 SQL"清单；顺带把连接是否可解析一起校验掉
pub fn previews(spec: &DatasetSpec, source: &DbSource) -> Result<Vec<SqlPreview>, String> {
    let cap = spec.row_cap();
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let push = &mut |alias: &str, seen: &mut Vec<String>, out: &mut Vec<SqlPreview>| -> Result<(), String> {
        if seen.iter().any(|a| a.eq_ignore_ascii_case(alias)) {
            return Ok(());
        }
        let src = spec
            .source(alias)
            .ok_or_else(|| format!("源 {} 不在 sources 里", alias))?;
        let (cfg, dialect) = source.resolve(src)?;
        out.push(SqlPreview {
            dataset: spec.id.clone(),
            alias: src.alias.clone(),
            connection_id: src.connection_id.clone(),
            connection_name: cfg.name.clone(),
            database_type: dialect.to_string(),
            sql: guarded_sql(&aligned(src, dialect), cap)?,
        });
        seen.push(alias.to_string());
        Ok(())
    };
    // 与 execute_dataset 同序：基表先，再按 join 顺序
    push(&spec.base, &mut seen, &mut out)?;
    for j in &spec.joins {
        push(&j.source, &mut seen, &mut out)?;
    }
    Ok(out)
}

/// 引擎表 → 与 columns 对齐的二维数组（跨 IPC 边界的唯一形态）
pub fn to_records(table: &Table) -> Vec<Vec<serde_json::Value>> {
    table
        .rows
        .iter()
        .map(|r| {
            table
                .columns
                .iter()
                .map(|c| r.get(c).cloned().unwrap_or(Value::Null).to_json())
                .collect()
        })
        .collect()
}

// ==================== Tauri 命令 ====================

/// 只跑计划，不连数据库之外的任何东西（除列清单探查）
#[tauri::command]
pub async fn report_dataset_validate(
    spec: DatasetSpec,
    configs: Vec<DBConfig>,
    schemas: Option<SchemaCache>,
) -> Result<ValidationReport, String> {
    let source = DbSource::new(&configs);
    let spec = align_dialects(&spec, &source)?;
    let cache = build_schemas(&spec, &source, schemas.as_ref()).await?;
    let steps = dataset::validate_dataset(&spec, &cache)?;
    Ok(ValidationReport {
        steps,
        sqls: previews(&spec, &source)?,
        schemas: cache,
    })
}

/// 纯本地：只要 spec 自带列清单就能出 SQL，不碰数据库
#[tauri::command]
pub async fn report_dataset_sql(
    spec: DatasetSpec,
    configs: Vec<DBConfig>,
) -> Result<Vec<SqlPreview>, String> {
    let source = DbSource::new(&configs);
    let spec = align_dialects(&spec, &source)?;
    previews(&spec, &source)
}

/// 校验 → 拉数 → 内存算子链，一次跑完
#[tauri::command]
pub async fn report_dataset_execute(
    spec: DatasetSpec,
    configs: Vec<DBConfig>,
    schemas: Option<SchemaCache>,
) -> Result<DatasetPayload, String> {
    let start = std::time::Instant::now();
    let source = DbSource::new(&configs);
    let spec = align_dialects(&spec, &source)?;
    let cache = build_schemas(&spec, &source, schemas.as_ref()).await?;
    let steps = dataset::validate_dataset(&spec, &cache)?;
    let sqls = previews(&spec, &source)?;
    let result = dataset::execute_dataset(&spec, &source).await?;
    Ok(DatasetPayload {
        columns: result.table.columns.clone(),
        rows: to_records(&result.table),
        row_count: result.table.len(),
        source_rows: result
            .source_rows
            .iter()
            .map(|(alias, rows)| SourceRowStat {
                alias: alias.clone(),
                rows: *rows,
            })
            .collect(),
        truncated: result.truncated.clone(),
        partial: result.is_partial(),
        generated_sql: sqls,
        steps,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

#[tauri::command]
pub async fn report_describe_columns(
    config: DBConfig,
    schema: String,
    table: String,
) -> Result<Vec<ColumnInfo>, String> {
    describe_columns_typed(&config, &schema, &table).await
}

// ==================== 报表（视图 × 多数据集） ====================

#[derive(Debug, Clone, Serialize)]
pub struct DatasetRunStat {
    pub id: String,
    pub name: String,
    pub rows: usize,
    pub columns: Vec<String>,
    pub truncated: Vec<String>,
    pub partial: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ViewPayload {
    /// 视图算子链（组件 × 编码 × 布局），前端"执行计划"面板直接列出来
    pub steps: Vec<String>,
    pub layout: Vec<view::WidgetLayout>,
    pub charts: Vec<view::ChartData>,
    pub datasets: Vec<DatasetRunStat>,
    pub generated_sql: Vec<SqlPreview>,
    /// 任一数据集被 max_rows 截断 → 整张报表标"不完整"，UI 不许当均值画
    pub partial: bool,
    pub elapsed_ms: u64,
}

/// 回填方言 → 探列 → 出计划，一行数据都不取。返回（数据集 id → 输出列、
/// 源 SQL、按连接真实方言补齐后的 spec）。
/// 校验只需要这三样：让"检查字段"顺手扫一遍库，是白付一次跨库取数。
async fn plan_datasets(
    datasets: &[DatasetSpec],
    source: &DbSource,
    provided: Option<&HashMap<String, SchemaCache>>,
) -> Result<(HashMap<String, Vec<String>>, Vec<SqlPreview>, Vec<DatasetSpec>), String> {
    let mut cols: HashMap<String, Vec<String>> = HashMap::new();
    let mut sqls: Vec<SqlPreview> = Vec::new();
    let mut aligned: Vec<DatasetSpec> = Vec::new();
    for ds in datasets {
        if ds.id.trim().is_empty() {
            return Err("数据集缺少 id".into());
        }
        if cols.contains_key(&ds.id) {
            return Err(format!("数据集 id {} 重复", ds.id));
        }
        let ds = align_dialects(ds, source)?;
        let owned = provided.and_then(|p| p.get(&ds.id));
        let cache = build_schemas(&ds, source, owned).await?;
        let plan = dataset::plan_dataset(&ds, &cache)?;
        sqls.extend(previews(&ds, source)?);
        cols.insert(ds.id.clone(), plan.columns);
        aligned.push(ds);
    }
    Ok((cols, sqls, aligned))
}

/// 逐个数据集：先出计划，再真取数。返回计划、结果表与生成的 SQL。
/// 计划里的列清单与结果表的列清单同源（plan_dataset 有回归测试锁住），
/// 所以视图校验报错指向的列一定是用户看得见的列。
async fn run_datasets(
    datasets: &[DatasetSpec],
    source: &DbSource,
    provided: Option<&HashMap<String, SchemaCache>>,
) -> Result<
    (
        HashMap<String, Table>,
        HashMap<String, Vec<String>>,
        Vec<DatasetRunStat>,
        Vec<SqlPreview>,
        bool,
    ),
    String,
> {
    let mut tables: HashMap<String, Table> = HashMap::new();
    let (cols, sqls, aligned) = plan_datasets(datasets, source, provided).await?;
    let mut stats: Vec<DatasetRunStat> = Vec::new();
    let mut partial = false;
    for ds in &aligned {
        let start = std::time::Instant::now();
        let run = dataset::execute_dataset(ds, source).await?;
        partial |= run.is_partial();
        stats.push(DatasetRunStat {
            id: ds.id.clone(),
            name: ds.name.clone(),
            rows: run.table.len(),
            columns: cols[&ds.id].clone(),
            truncated: run.truncated.clone(),
            partial: run.is_partial(),
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
        tables.insert(ds.id.clone(), run.table);
    }
    Ok((tables, cols, stats, sqls, partial))
}

/// 只校验不出数：AI 生成完整报表后先用它自检，能省下一次全量取数
#[tauri::command]
pub async fn report_view_validate(
    view: ViewSpec,
    datasets: Vec<DatasetSpec>,
    configs: Vec<DBConfig>,
    schemas: Option<HashMap<String, SchemaCache>>,
) -> Result<ValidationReport, String> {
    let source = DbSource::new(&configs);
    let (cols, sqls, _) = plan_datasets(&datasets, &source, schemas.as_ref()).await?;
    let steps = view::validate_view(&view, &cols)?;
    Ok(ValidationReport {
        steps,
        sqls,
        // 这里回传每个数据集的输出列，前端据此渲染字段选择器
        schemas: cols,
    })
}

/// 出数并渲染成图表结构
#[tauri::command]
pub async fn report_view_render(
    view: ViewSpec,
    datasets: Vec<DatasetSpec>,
    configs: Vec<DBConfig>,
    schemas: Option<HashMap<String, SchemaCache>>,
) -> Result<ViewPayload, String> {
    let start = std::time::Instant::now();
    let source = DbSource::new(&configs);
    let (tables, cols, stats, sqls, partial) =
        run_datasets(&datasets, &source, schemas.as_ref()).await?;
    let steps = view::validate_view(&view, &cols)?;
    let layout = view::resolve_layout(&view)?;
    let charts = view::render_view(&view, &tables)?;
    Ok(ViewPayload {
        steps,
        layout,
        charts,
        datasets: stats,
        generated_sql: sqls,
        partial,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::dataset;
    use crate::report::dataset::{JoinPair, JoinSpec, SortDir};
    use crate::report::table::{AggFunc, AggSpec};

    fn col_types(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn sqlite_cfg(id: &str, path: &str) -> DBConfig {
        DBConfig {
            id: id.into(),
            name: id.into(),
            db_type: "sqlite".into(),
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            database: path.into(),
        }
    }

    fn src(alias: &str, conn: &str, table: &str) -> SourceRef {
        SourceRef {
            alias: alias.into(),
            connection_id: conn.into(),
            database_type: "sqlite".into(),
            schema: String::new(),
            table: table.into(),
            columns: Vec::new(),
        }
    }

    /// 一个临时目录里的两个独立 SQLite 文件：两条连接、两个连接池，
    /// 是真跨库而不是同库两表。
    struct TempDb {
        dir: std::path::PathBuf,
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_db() -> TempDb {
        let dir = std::env::temp_dir().join(format!("zdb-report-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        TempDb { dir }
    }

    /// sqlx 不代建 SQLite 文件，所以测试自己 touch 出空库
    fn path_in(db: &TempDb, name: &str) -> String {
        let path = db.dir.join(name);
        std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("建空 SQLite 文件");
        path.to_string_lossy().to_string()
    }

    #[test]
    fn text_lit_blocks_quote_and_backslash_escape() {
        assert_eq!(text_lit("O'Brien").unwrap(), "'O''Brien'");
        // MySQL 默认把 \ 当转义符：\' 会吃掉收尾引号，所以整串拒绝
        assert!(text_lit(r"a\' OR 1=1 --").is_err());
        assert!(text_lit("a\0b").is_err());
        assert!(text_lit("正常中文库").is_ok());
    }

    /// 回归两件事：① 给 sqlite 加"文件不存在"提示时不能把 :memory: 一起挡掉；
    /// ② 记下真实限制——每条语句新建一个池，`:memory:` 因此每次都是全新的空库，
    /// 报表/探查只能走文件库。连接亲和性属于 T-012 会话 actor 的活。
    #[tokio::test]
    async fn in_memory_sqlite_skips_path_check_but_shares_nothing() {
        let cfg = sqlite_cfg("mem", ":memory:");
        run_dispatch(&cfg, "CREATE TABLE t (id INTEGER)").await.unwrap();
        let err = describe_columns(&cfg, "", "t").await.unwrap_err();
        assert!(err.contains("没有可读取的列"), "{}", err);
        assert!(!err.contains("文件不存在"), "{}", err);
    }

    #[tokio::test]
    async fn missing_sqlite_file_says_why() {
        // 回归：sqlx 只回 "unable to open database file"，用户看不出是路径写错
        let cfg = sqlite_cfg("ghost", "/tmp/definitely-not-here-zdb-42/case.sqlite");
        let err = run_dispatch(&cfg, "SELECT 1").await.unwrap_err();
        assert!(err.contains("SQLite 文件不存在"), "{}", err);
        assert!(err.contains("/tmp/definitely-not-here-zdb-42/case.sqlite"), "{}", err);
    }

    #[test]
    fn describe_sql_shape_per_dialect() {
        assert_eq!(
            describe_sql("sqlite", "", "users", "").unwrap(),
            "SELECT name, type FROM pragma_table_info('users') ORDER BY cid"
        );
        let pg = describe_sql("postgresql", "", "users", "app").unwrap();
        assert!(pg.contains("table_schema = 'public'"), "{}", pg);
        // 三种方言都必须第二列给类型，否则提示词里就只有裸列名
        assert!(pg.contains("SELECT column_name, data_type"), "{}", pg);
        let my = describe_sql("mysql", "", "users", "shop").unwrap();
        assert!(my.contains("TABLE_SCHEMA = 'shop'"), "{}", my);
        // MySQL 用 COLUMN_TYPE 而不是 DATA_TYPE：decimal(12,2) 的精度才留得住
        assert!(my.contains("SELECT COLUMN_NAME, COLUMN_TYPE"), "{}", my);
        // 表名仍走标识符白名单
        assert!(describe_sql("mysql", "", "users; DROP x", "shop").is_err());
        assert!(describe_sql("oracle", "", "users", "").is_err());
    }

    #[test]
    fn generated_source_sql_passes_readonly_gate() {
        let s = src("u", "c1", "users");
        let sql = guarded_sql(&s, 100).expect("只读 SELECT 应通过");
        assert_eq!(sql, "SELECT * FROM \"users\" LIMIT 101");
        assert_eq!(classify(&sql).safety, SafetyClass::ReadOnlySafe);
    }

    #[test]
    fn injection_attempt_never_reaches_the_gate() {
        // 故障注入：把语句拼进表名，标识符白名单必须先挡住，
        // 不能指望分类器兜底（分类器只在白名单被放宽时才是第二道门）
        let evil = src("u", "c1", "users\" ; DROP TABLE users; --");
        let err = guarded_sql(&evil, 10).unwrap_err();
        assert!(err.contains("非法字符"), "{}", err);
        let union = src("u", "c1", "users UNION SELECT password FROM mysql.user");
        assert!(guarded_sql(&union, 10).is_err());
    }

    #[tokio::test]
    async fn unknown_connection_is_named_in_the_error() {
        let db = temp_db();
        let cfg = sqlite_cfg("known", &path_in(&db, "known.sqlite"));
        run_dispatch(&cfg, "CREATE TABLE t (id INTEGER)").await.unwrap();
        let source = DbSource::new(&[cfg]);
        let spec = DatasetSpec {
            id: "d".into(),
            name: "d".into(),
            base: "t".into(),
            sources: vec![src("t", "missing-one", "t")],
            ..DatasetSpec::new("d", "d", "t")
        };
        let err = dataset::execute_dataset(&spec, &source)
            .await
            .unwrap_err();
        assert!(err.contains("missing-one"), "{}", err);
        assert!(err.contains("不在本会话"), "{}", err);
    }

    #[tokio::test]
    async fn dialect_mismatch_is_rejected_before_connecting() {
        let db = temp_db();
        let cfg = sqlite_cfg("lite", &path_in(&db, "lite.sqlite"));
        run_dispatch(&cfg, "CREATE TABLE t (id INTEGER)").await.unwrap();
        let source = DbSource::new(&[cfg]);
        let mut s = src("t", "lite", "t");
        s.database_type = "mysql".into();
        let err = source.fetch(&s, 10).await.unwrap_err();
        assert!(err.contains("声明方言 mysql"), "{}", err);
        assert!(err.contains("实际是 sqlite"), "{}", err);
    }

    #[tokio::test]
    async fn describe_columns_reads_real_sqlite_columns() {
        let db = temp_db();
        let cfg = sqlite_cfg("lite", &path_in(&db, "lite.sqlite"));
        // 最后一列故意不写类型：sqlite 允许，类型必须是空串而不是把整次探查打回
        run_dispatch(
            &cfg,
            "CREATE TABLE cust (id INTEGER, name TEXT, score REAL, note)",
        )
        .await
        .unwrap();
        let cols = describe_columns(&cfg, "", "cust").await.unwrap();
        assert_eq!(cols, vec!["id", "name", "score", "note"]);
        let typed = describe_columns_typed(&cfg, "", "cust").await.unwrap();
        assert_eq!(
            typed
                .iter()
                .map(|c| (c.name.as_str(), c.data_type.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("id", "INTEGER"),
                ("name", "TEXT"),
                ("score", "REAL"),
                ("note", "")
            ]
        );
        // 不存在的表必须明确报错，而不是回一个空清单让校验"意外通过"
        assert!(describe_columns(&cfg, "", "ghost").await.is_err());
    }

    /// 两个独立 SQLite 文件：订单在 shop 库、客户在 crm 库。
    /// 两条连接、两个连接池，是真跨库而不是同库两表。
    async fn seed_shop_crm(db: &TempDb) -> (DBConfig, DBConfig) {
        let shop = sqlite_cfg("shop", &path_in(db, "shop.sqlite"));
        let crm = sqlite_cfg("crm", &path_in(db, "crm.sqlite"));
        run_dispatch(&shop, "CREATE TABLE orders (id INTEGER, user_id INTEGER, amount REAL, status TEXT)")
            .await
            .unwrap();
        for (id, uid, amt, st) in [
            (1, 1, 100.0, "paid"),
            (2, 1, 50.0, "paid"),
            (3, 2, 80.0, "paid"),
            (4, 2, 500.0, "refunded"),
            (5, 3, 70.0, "paid"),
        ] {
            run_dispatch(
                &shop,
                &format!("INSERT INTO orders VALUES ({}, {}, {}, '{}')", id, uid, amt, st),
            )
            .await
            .unwrap();
        }
        run_dispatch(&crm, "CREATE TABLE users (id INTEGER, city TEXT)")
            .await
            .unwrap();
        for (id, city) in [(1, "SH"), (2, "BJ"), (3, "SZ")] {
            run_dispatch(&crm, &format!("INSERT INTO users VALUES ({}, '{}')", id, city))
                .await
                .unwrap();
        }
        (shop, crm)
    }

    fn city_gmv_spec() -> DatasetSpec {
        DatasetSpec {
            id: "city-gmv".into(),
            name: "城市成交额".into(),
            base: "o".into(),
            sources: vec![src("o", "shop", "orders"), src("u", "crm", "users")],
            joins: vec![JoinSpec {
                source: "u".into(),
                on: vec![JoinPair {
                    left: "user_id".into(),
                    right: "id".into(),
                }],
                kind: Default::default(),
            }],
            filters: vec!["status = 'paid'".into()],
            group_by: vec!["city".into()],
            aggregates: vec![
                AggSpec::new("gmv", AggFunc::Sum, Some("amount")),
                AggSpec::new("cnt", AggFunc::Count, None),
            ],
            sort: vec![SortDir {
                column: "gmv".into(),
                desc: true,
            }],
            ..DatasetSpec::new("city-gmv", "城市成交额", "o")
        }
    }

    /// 端到端：两个 SQLite 文件联邦 + join + 过滤 + 聚合 + 排序
    #[tokio::test]
    async fn federates_two_sqlite_files_and_aggregates() {
        let db = temp_db();
        let (shop, crm) = seed_shop_crm(&db).await;
        let spec = city_gmv_spec();

        let source = DbSource::new(&[shop.clone(), crm.clone()]);
        // 列清单留空 → 走真实探查，证明 pragma/information_schema 通道可用
        let cache = build_schemas(&spec, &source, None).await.unwrap();
        assert_eq!(
            cache.get("o").unwrap(),
            &vec!["id".to_string(), "user_id".into(), "amount".into(), "status".into()]
        );
        let steps = dataset::validate_dataset(&spec, &cache).unwrap();
        assert!(steps.iter().any(|s| s.starts_with("INNER JOIN u")), "{:?}", steps);

        let payload = report_dataset_execute(spec, vec![shop, crm], None)
            .await
            .unwrap();
        assert_eq!(payload.row_count, 3, "{:?}", payload.rows);
        assert!(!payload.partial);
        assert_eq!(payload.columns, vec!["city", "gmv", "cnt"]);
        // SH=150.0 居首（两单 paid），BJ=80.0，SZ=70.0
        assert_eq!(payload.rows[0][0], serde_json::json!("SH"));
        assert_eq!(payload.rows[0][1], serde_json::json!(150.0));
        assert_eq!(payload.rows[0][2], serde_json::json!(2));
        assert_eq!(payload.rows[1][0], serde_json::json!("BJ"));
        assert_eq!(payload.rows[2][0], serde_json::json!("SZ"));
        // 每条源 SQL 都可在"查看生成的 SQL"里回显
        assert_eq!(payload.generated_sql.len(), 2);
        assert_eq!(payload.generated_sql[0].connection_name, "shop");
        assert!(payload.generated_sql[1].sql.starts_with("SELECT * FROM \"users\" LIMIT"));
    }

    #[tokio::test]
    async fn execute_surfaces_row_cap_as_partial() {
        let db = temp_db();
        let cfg = sqlite_cfg("bulk", &path_in(&db, "bulk.sqlite"));
        run_dispatch(&cfg, "CREATE TABLE t (id INTEGER)").await.unwrap();
        for i in 0..12 {
            run_dispatch(&cfg, &format!("INSERT INTO t VALUES ({})", i))
                .await
                .unwrap();
        }
        let spec = DatasetSpec {
            id: "cap".into(),
            name: "cap".into(),
            base: "t".into(),
            sources: vec![src("t", "bulk", "t")],
            max_rows: Some(5),
            ..DatasetSpec::new("cap", "cap", "t")
        };
        let payload = report_dataset_execute(spec, vec![cfg], None).await.unwrap();
        assert!(payload.partial, "截断必须被显式标记，否则 UI 会把半份数据当真");
        assert_eq!(payload.truncated, vec!["t"]);
        assert_eq!(payload.source_rows[0].rows, 6, "多取 1 行用来判定截断");
        assert_eq!(payload.row_count, 5);
    }

    #[tokio::test]
    async fn validate_reports_hallucinated_column_before_touching_rows() {
        let db = temp_db();
        let cfg = sqlite_cfg("lite", &path_in(&db, "lite.sqlite"));
        run_dispatch(&cfg, "CREATE TABLE t (id INTEGER, amount REAL)")
            .await
            .unwrap();
        let spec = DatasetSpec {
            id: "bad".into(),
            name: "bad".into(),
            base: "t".into(),
            sources: vec![src("t", "lite", "t")],
            filters: vec!["total_fee > 10".into()],
            ..DatasetSpec::new("bad", "bad", "t")
        };
        let err = report_dataset_validate(spec, vec![cfg], None)
            .await
            .unwrap_err();
        assert!(err.contains("total_fee"), "{}", err);
    }

    fn widget(
        id: &str,
        kind: view::ChartType,
        dataset_id: &str,
        encode: view::WidgetEncode,
    ) -> view::WidgetSpec {
        view::WidgetSpec {
            id: id.into(),
            kind,
            title: id.into(),
            dataset: dataset_id.into(),
            encode,
            agg: Default::default(),
            filters: Vec::new(),
            limit: None,
        }
    }

    /// 单库明细数据集，和城市聚合集一起放进同一张报表
    fn paid_orders_spec() -> DatasetSpec {
        DatasetSpec {
            sources: vec![src("o", "shop", "orders")],
            filters: vec!["status = 'paid'".into()],
            fields: vec!["id".into(), "amount".into()],
            sort: vec![SortDir {
                column: "id".into(),
                desc: true,
            }],
            limit: Some(2),
            ..DatasetSpec::new("paid-orders", "已支付订单明细", "o")
        }
    }

    fn shop_board() -> ViewSpec {
        let kpi = widget(
            "kpi-gmv",
            view::ChartType::Kpi,
            "city-gmv",
            view::WidgetEncode {
                y: Some("gmv".into()),
                ..Default::default()
            },
        );
        let bar = widget(
            "bar-city",
            view::ChartType::Bar,
            "city-gmv",
            view::WidgetEncode {
                x: Some("city".into()),
                y: Some("gmv".into()),
                ..Default::default()
            },
        );
        let table = widget(
            "tbl-orders",
            view::ChartType::Table,
            "paid-orders",
            view::WidgetEncode {
                columns: vec!["id".into(), "amount".into()],
                ..Default::default()
            },
        );
        ViewSpec {
            id: "shop-board".into(),
            name: "成交看板".into(),
            version: 1,
            widgets: vec![kpi, bar, table],
            layout: Vec::new(),
        }
    }

    /// 端到端：一次 render 跑通"跨库聚合 + 单库明细"两个数据集、三类组件
    #[tokio::test]
    async fn report_view_renders_every_widget_from_cross_db_datasets() {
        let db = temp_db();
        let (shop, crm) = seed_shop_crm(&db).await;
        let datasets = || vec![city_gmv_spec(), paid_orders_spec()];

        let report = report_view_validate(
            shop_board(),
            datasets(),
            vec![shop.clone(), crm.clone()],
            None,
        )
        .await
        .unwrap();
        assert_eq!(report.schemas["city-gmv"], vec!["city", "gmv", "cnt"]);
        assert_eq!(report.schemas["paid-orders"], vec!["id", "amount"]);
        // 跨库集 2 条源 SQL + 明细集 1 条
        assert_eq!(report.sqls.len(), 3);
        assert_eq!(report.sqls[2].dataset, "paid-orders");

        let payload = report_view_render(shop_board(), datasets(), vec![shop, crm], None)
            .await
            .unwrap();
        assert!(!payload.partial);
        assert_eq!(payload.charts.len(), 3);
        // KPI 不写 agg 就是 SUM：三城 150+80+70
        assert_eq!(payload.charts[0].value, Some(Value::Float(300.0)));
        // 柱状不写 agg 就原样画三行，不会悄悄压成一个点
        assert_eq!(
            payload.charts[1].categories,
            vec!["SH".to_string(), "BJ".into(), "SZ".into()]
        );
        assert_eq!(
            payload.charts[1].series[0].values,
            vec![Value::Float(150.0), Value::Float(80.0), Value::Float(70.0)]
        );
        // 表格吃明细集：按 id 倒序取前 2
        assert_eq!(payload.charts[2].columns, vec!["id", "amount"]);
        assert_eq!(payload.charts[2].rows.len(), 2);
        assert_eq!(payload.charts[2].rows[0][0], serde_json::json!(5));
        // layout 留空 → 自动纵向堆叠，且互不重叠
        assert_eq!(payload.layout.len(), 3);
        assert_eq!(payload.datasets.len(), 2);
        for pair in payload.layout.windows(2) {
            assert!(pair[0].y + pair[0].h <= pair[1].y, "{:?}", payload.layout);
        }
        // 前端要能直接吃这个结构：值必须是标量，不能是 {"Float":150.0} 这种外部标签
        let wire = serde_json::to_value(&payload).expect("ViewPayload 可序列化");
        assert_eq!(wire["charts"][0]["value"], serde_json::json!(300.0));
        assert_eq!(
            wire["charts"][1]["series"][0]["values"],
            serde_json::json!([150.0, 80.0, 70.0])
        );
        assert_eq!(wire["charts"][2]["rows"][0][0], serde_json::json!(5));
    }

    /// AI 编了个不存在的度量：报错要指名道姓，而不是回一张空图
    /// 省略 database_type 的 spec（前端 designer 手搓、AI 只写三个字段）也要能跑，
    /// 方言由连接真实值回填，而不是报一句看不懂的"声明方言 "。
    #[tokio::test]
    async fn omitted_dialect_is_filled_from_the_connection() {
        let db = temp_db();
        let cfg = sqlite_cfg("lite", &path_in(&db, "lite.sqlite"));
        run_dispatch(&cfg, "CREATE TABLE t (id INTEGER, amount REAL)")
            .await
            .unwrap();
        run_dispatch(&cfg, "INSERT INTO t VALUES (1, 12.5)").await.unwrap();
        let mut s = src("t", "lite", "t");
        s.database_type = String::new();
        let spec = DatasetSpec {
            id: "d".into(),
            name: "d".into(),
            base: "t".into(),
            sources: vec![s],
            ..DatasetSpec::new("d", "d", "t")
        };
        let source = DbSource::new(&[cfg]);
        let payload = report_dataset_execute(spec.clone(), vec![sqlite_cfg("lite", &path_in(&db, "lite.sqlite"))], None)
            .await
            .unwrap();
        assert_eq!(payload.row_count, 1);
        let sqls = previews(&spec, &source).unwrap();
        assert_eq!(sqls[0].database_type, "sqlite", "回显的方言必须是连接真实方言");
        assert!(sqls[0].sql.starts_with("SELECT * FROM \"t\" LIMIT"), "{}", sqls[0].sql);
    }

    /// 全链路收口：模型只写 alias/connection_id/table（不带方言、不带列清单），
    /// 本地校验补全后直接喂给真实的两文件跨库渲染。
    #[tokio::test]
    async fn ai_draft_feeds_a_real_cross_db_render() {
        use crate::report::ai::{self, CatalogTable};
        let db = temp_db();
        let (shop, crm) = seed_shop_crm(&db).await;
        let catalog = vec![
            CatalogTable {
                connection_id: "shop".into(),
                connection_name: "商城库".into(),
                database_type: "sqlite".into(),
                schema: String::new(),
                table: "orders".into(),
                columns: vec!["id".into(), "user_id".into(), "amount".into(), "status".into()],
                column_types: col_types(&[("id", "INTEGER"), ("user_id", "INTEGER"), ("amount", "REAL"), ("status", "TEXT")]),
            },
            CatalogTable {
                connection_id: "crm".into(),
                connection_name: "客户库".into(),
                database_type: "sqlite".into(),
                schema: String::new(),
                table: "users".into(),
                columns: vec!["id".into(), "city".into()],
                column_types: col_types(&[("id", "INTEGER"), ("city", "TEXT")]),
            },
        ];
        // 提示词里带类型，本机校验比对的仍只是列名
        assert!(ai::prompt("各城市成交额", &catalog, None).contains("amount REAL"));
        let raw = r#"好的，这是你要的看板：
        ```json
        {"datasets":[{"id":"city-gmv","name":"城市成交额","base":"o",
          "sources":[{"alias":"o","connection_id":"shop","table":"orders"},
                     {"alias":"u","connection_id":"crm","table":"users"}],
          "joins":[{"source":"u","on":[{"left":"user_id","right":"id"}]}],
          "filters":["status = 'paid'"],"group_by":["city"],
          "aggregates":[{"output":"gmv","func":"SUM","column":"amount"}],
          "sort":[{"column":"gmv","desc":true}]}],
         "view":{"id":"board","name":"成交看板","widgets":[
           {"id":"kpi","type":"KPI","title":"总成交额","dataset":"city-gmv","encode":{"y":"gmv"}},
           {"id":"bar","type":"BAR","title":"分城市","dataset":"city-gmv","encode":{"x":"city","y":"gmv"}}
         ]}}
        ```
        希望有帮助。"#;
        let draft = ai::check_draft(ai::parse_draft(raw).unwrap(), &catalog).unwrap();
        assert_eq!(draft.datasets[0].sources[1].database_type, "sqlite");
        assert_eq!(draft.columns["city-gmv"], vec!["city", "gmv"]);

        let payload = report_view_render(
            draft.view,
            draft.datasets,
            vec![shop, crm],
            None,
        )
        .await
        .unwrap();
        assert!(!payload.partial);
        assert_eq!(payload.charts[0].value, Some(Value::Float(300.0)));
        assert_eq!(
            payload.charts[1].categories,
            vec!["SH".to_string(), "BJ".into(), "SZ".into()]
        );
        // 两个库各一条源 SQL，模型从没写过任何 SQL
        assert_eq!(payload.generated_sql.len(), 2);
    }

    /// 连接键类型族告警不是玄学：同一份草稿只换连接键，
    /// 空表就正按告警说的那样发生；换回同族则三行照出。
    #[tokio::test]
    async fn warning_predicts_the_silently_empty_cross_db_join() {
        use crate::report::ai::{self, CatalogTable};
        let db = temp_db();
        let (shop, crm) = seed_shop_crm(&db).await;
        // 网页端的 uid 是文本：值一模一样，桶不一样（'1' → S1，1 → #1.0）
        run_dispatch(&crm, "CREATE TABLE web_users (uid TEXT, city TEXT)")
            .await
            .unwrap();
        for (uid, city) in [("1", "SH"), ("2", "BJ"), ("3", "SZ")] {
            run_dispatch(
                &crm,
                &format!("INSERT INTO web_users VALUES ('{}', '{}')", uid, city),
            )
            .await
            .unwrap();
        }
        let orders = CatalogTable {
            connection_id: "shop".into(),
            connection_name: "商城库".into(),
            database_type: "sqlite".into(),
            schema: String::new(),
            table: "orders".into(),
            columns: vec!["id".into(), "user_id".into(), "amount".into(), "status".into()],
            column_types: col_types(&[
                ("id", "INTEGER"),
                ("user_id", "INTEGER"),
                ("amount", "REAL"),
                ("status", "TEXT"),
            ]),
        };
        let right = |table: &str, cols: &[(&str, &str)]| CatalogTable {
            connection_id: "crm".into(),
            connection_name: "客户库".into(),
            database_type: "sqlite".into(),
            schema: String::new(),
            table: table.into(),
            columns: cols.iter().map(|(n, _)| n.to_string()).collect(),
            column_types: col_types(cols),
        };
        let raw = |table: &str, key: &str| {
            format!(
                r#"{{"datasets":[{{"id":"city-gmv","name":"城市成交额","base":"o",
                  "sources":[{{"alias":"o","connection_id":"shop","table":"orders"}},
                              {{"alias":"u","connection_id":"crm","table":"{}"}}],
                  "joins":[{{"source":"u","on":[{{"left":"user_id","right":"{}"}}]}}],
                  "filters":["status = 'paid'"],"group_by":["city"],
                  "aggregates":[{{"output":"gmv","func":"SUM","column":"amount"}}]}}],
                 "view":{{"id":"board","name":"成交看板","widgets":[
                   {{"id":"bar","type":"BAR","title":"分城市","dataset":"city-gmv",
                     "encode":{{"x":"city","y":"gmv"}}}}
                 ]}}}}"#,
                table, key
            )
        };

        let bad = ai::check_draft(
            ai::parse_draft(&raw("web_users", "uid")).unwrap(),
            &[orders.clone(), right("web_users", &[("uid", "TEXT"), ("city", "TEXT")])],
        )
        .unwrap();
        assert_eq!(bad.warnings.len(), 1, "{:?}", bad.warnings);
        assert!(bad.warnings[0].contains("INTEGER"), "{}", bad.warnings[0]);
        assert!(bad.warnings[0].contains("TEXT"), "{}", bad.warnings[0]);
        let bad_payload = report_view_render(bad.view, bad.datasets, vec![shop.clone(), crm.clone()], None)
            .await
            .unwrap();
        assert!(
            bad_payload.charts[0].categories.is_empty(),
            "告警说这一轮 join 配不上，结果它画出了图——告警在瞎报"
        );

        // 同族连接键：既不告警，也必须真的出三行，否则上面的"空"不作数
        let good = ai::check_draft(
            ai::parse_draft(&raw("users", "id")).unwrap(),
            &[orders, right("users", &[("id", "INTEGER"), ("city", "TEXT")])],
        )
        .unwrap();
        assert!(good.warnings.is_empty(), "{:?}", good.warnings);
        let good_payload = report_view_render(good.view, good.datasets, vec![shop, crm], None)
            .await
            .unwrap();
        assert_eq!(
            good_payload.charts[0].categories.len(),
            3,
            "种子数据没生效的话，上面那条「空表」就不能算被证明"
        );
    }

    #[tokio::test]
    async fn report_view_names_the_hallucinated_field() {
        let db = temp_db();
        let (shop, crm) = seed_shop_crm(&db).await;
        let board = shop_board();
        let mut broken = board.widgets[0].clone();
        broken.encode = view::WidgetEncode {
            y: Some("total_fee".into()),
            ..Default::default()
        };
        let v = ViewSpec {
            widgets: vec![broken],
            ..board
        };
        let err = report_view_render(v, vec![city_gmv_spec()], vec![shop, crm], None)
            .await
            .unwrap_err();
        assert!(err.contains("total_fee"), "{}", err);
        assert!(err.contains("kpi-gmv"), "{}", err);
    }
}
