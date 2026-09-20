use serde::{Deserialize, Serialize};
use tauri::command;

use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlRow};
use sqlx::postgres::{PgConnectOptions, PgPool, PgRow};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqliteRow};
use sqlx::{Column, Row, TypeInfo, ValueRef};

#[path = "queries.rs"]
mod queries;

// ================== 数据结构 ==================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DBConfig {
    pub id: String,
    pub name: String,
    pub db_type: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub id: String,  // 添加 id 字段
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub affected_rows: u64,
    pub execution_time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    pub schema: Option<String>,
    pub row_estimate: Option<u64>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub is_primary: bool,
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryHistoryItem {
    pub id: String,
    pub sql: String,
    pub connection_id: String,
    pub connection_name: String,
    pub timestamp: i64,
    pub execution_time_ms: u64,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    pub sql: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedConnection {
    pub name: String,
    pub db_type: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub database: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionExport {
    pub version: u32,
    pub exported_at: i64,
    pub connections: Vec<ExportedConnection>,
}

// ================== AI 配置 ==================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIRequest {
    pub prompt: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIResponse {
    pub success: bool,
    pub content: String,
    pub error: Option<String>,
}

// 前端连接配置的落盘结构（与前端 DBConnection 对应，字段 type）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", alias = "db_type", default)]
    pub db_type: String,
    #[serde(default)]
    pub host: String,
    #[serde(default, deserialize_with = "de_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub database: String,
}

// 端口容错：允许数字或字符串
fn de_port<'de, D>(d: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Number(n) => Ok(n.as_u64().unwrap_or(0) as u16),
        serde_json::Value::String(s) => Ok(s.trim().parse().unwrap_or(0)),
        _ => Ok(0),
    }
}

// host 误填成 "host:port" 时自动拆分（不影响 IPv6 字面量）
fn split_host_port(host: &str, default_port: u16) -> (String, u16) {
    let h = host.trim();
    if let Some((hpart, ppart)) = h.rsplit_once(':') {
        let has_bracket = hpart.contains(']');
        if !has_bracket && !ppart.is_empty() && ppart.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(p) = ppart.parse::<u16>() {
                if p > 0 {
                    return (hpart.to_string(), p);
                }
            }
        }
    }
    (h.to_string(), default_port)
}

// ================== 连接辅助 ==================

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const POOL_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn mysql_pool(cfg: &DBConfig) -> Result<MySqlPool, String> {
    let (host, port) = split_host_port(&cfg.host, cfg.port);
    let opts = MySqlConnectOptions::new()
        .host(&host)
        .port(port)
        .username(&cfg.username)
        .password(&cfg.password)
        .database(&cfg.database);
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        sqlx::pool::PoolOptions::<sqlx::MySql>::new()
            .acquire_timeout(POOL_CONNECT_TIMEOUT)
            .connect_with(opts),
    )
        .await
        .map_err(|_| {
            "连接超时（15 秒）：网络不可达或防火墙丢包，请检查 VPN / 安全组 / 主机端口".to_string()
        })?
        .map_err(|e| e.to_string())
}

async fn pg_pool(cfg: &DBConfig) -> Result<PgPool, String> {
    let (host, port) = split_host_port(&cfg.host, cfg.port);
    let opts = PgConnectOptions::new()
        .host(&host)
        .port(port)
        .username(&cfg.username)
        .password(&cfg.password)
        .database(&cfg.database);
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        sqlx::pool::PoolOptions::<sqlx::Postgres>::new()
            .acquire_timeout(POOL_CONNECT_TIMEOUT)
            .connect_with(opts),
    )
        .await
        .map_err(|_| {
            "连接超时（15 秒）：网络不可达或防火墙丢包，请检查 VPN / 安全组 / 主机端口".to_string()
        })?
        .map_err(|e| e.to_string())
}

async fn sqlite_pool(cfg: &DBConfig) -> Result<SqlitePool, String> {
    if cfg.database.trim().is_empty() {
        return Err("SQLite 需要在「数据库名」中填写数据库文件路径".to_string());
    }
    let opts = SqliteConnectOptions::new().filename(cfg.database.trim());
    tokio::time::timeout(CONNECT_TIMEOUT, SqlitePool::connect_with(opts))
        .await
        .map_err(|_| "SQLite 打开失败或文件不可读".to_string())?
        .map_err(|e| e.to_string())
}

fn dispatch_db_type(cfg: &DBConfig) -> Result<&'static str, String> {
    match cfg.db_type.as_str() {
        "mysql" => Ok("mysql"),
        "postgresql" => Ok("postgresql"),
        "sqlite" => Ok("sqlite"),
        "sqlserver" => Err("SQL Server 暂不支持，请使用 MySQL / PostgreSQL / SQLite".to_string()),
        other => Err(format!("不支持的数据库类型: {}", other)),
    }
}

// 是否为返回结果集的语句
fn is_query_stmt(sql: &str) -> bool {
    let s = sql.trim_start().to_lowercase();
    s.starts_with("select")
        || s.starts_with("with")
        || s.starts_with("show")
        || s.starts_with("desc")
        || s.starts_with("explain")
        || s.starts_with("pragma")
}

// ================== 通用取值 ==================

macro_rules! define_dec {
    ($fn_name:ident, $driver:ty, $row:ty) => {
        fn $fn_name<T>(row: &$row, i: usize) -> Option<serde_json::Value>
        where
            T: for<'r> sqlx::Decode<'r, $driver>
                + sqlx::Type<$driver>
                + serde::Serialize
                + std::marker::Send
                + std::marker::Sync,
        {
            match row.try_get::<Option<T>, _>(i) {
                Ok(Some(v)) => serde_json::to_value(v).ok(),
                _ => None,
            }
        }
    };
}

define_dec!(mysql_dec, sqlx::MySql, MySqlRow);
define_dec!(pg_dec, sqlx::Postgres, PgRow);
define_dec!(sqlite_dec, sqlx::Sqlite, SqliteRow);

fn bytes_to_value(v: Option<Vec<u8>>) -> Option<serde_json::Value> {
    v.map(|b| serde_json::Value::String(String::from_utf8_lossy(&b).to_string()))
}

fn mysql_cell(row: &MySqlRow, i: usize) -> serde_json::Value {
    let raw = match row.try_get_raw(i) {
        Ok(v) => v,
        Err(_) => return serde_json::Value::Null,
    };
    if raw.is_null() {
        return serde_json::Value::Null;
    }
    let v = match raw.type_info().name() {
        "BOOLEAN" => mysql_dec::<bool>(row, i).or_else(|| mysql_dec::<i8>(row, i)),
        "TINYINT" => mysql_dec::<i8>(row, i),
        "TINYINT UNSIGNED" => mysql_dec::<u8>(row, i),
        "SMALLINT" | "YEAR" => mysql_dec::<i16>(row, i),
        "SMALLINT UNSIGNED" => mysql_dec::<u16>(row, i),
        "INT" | "MEDIUMINT" => mysql_dec::<i32>(row, i),
        "INT UNSIGNED" | "MEDIUMINT UNSIGNED" => mysql_dec::<u32>(row, i),
        "BIGINT" => mysql_dec::<i64>(row, i),
        "BIGINT UNSIGNED" | "BIT" => mysql_dec::<u64>(row, i),
        "FLOAT" | "DOUBLE" => mysql_dec::<f64>(row, i),
        "DECIMAL" => mysql_dec::<sqlx::types::Decimal>(row, i),
        "DATE" => mysql_dec::<chrono::NaiveDate>(row, i),
        "TIME" => mysql_dec::<chrono::NaiveTime>(row, i),
        "DATETIME" | "TIMESTAMP" => mysql_dec::<chrono::NaiveDateTime>(row, i),
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" | "GEOMETRY" => {
            bytes_to_value(
                row.try_get::<Option<Vec<u8>>, _>(i)
                    .ok()
                    .flatten(),
            )
        }
        _ => mysql_dec::<String>(row, i),
    };
    v.unwrap_or(serde_json::Value::Null)
}

fn pg_cell(row: &PgRow, i: usize) -> serde_json::Value {
    let raw = match row.try_get_raw(i) {
        Ok(v) => v,
        Err(_) => return serde_json::Value::Null,
    };
    if raw.is_null() {
        return serde_json::Value::Null;
    }
    let v = match raw.type_info().name() {
        "bool" => pg_dec::<bool>(row, i),
        "int2" => pg_dec::<i16>(row, i),
        "int4" | "oid" => pg_dec::<i32>(row, i),
        "int8" => pg_dec::<i64>(row, i),
        "float4" => pg_dec::<f32>(row, i),
        "float8" => pg_dec::<f64>(row, i),
        "numeric" => pg_dec::<sqlx::types::Decimal>(row, i),
        "date" => pg_dec::<chrono::NaiveDate>(row, i),
        "time" => pg_dec::<chrono::NaiveTime>(row, i),
        "timestamp" => pg_dec::<chrono::NaiveDateTime>(row, i),
        "timestamptz" => pg_dec::<chrono::DateTime<chrono::Utc>>(row, i),
        "uuid" => pg_dec::<sqlx::types::Uuid>(row, i),
        "json" | "jsonb" => pg_dec::<serde_json::Value>(row, i),
        "bytea" => bytes_to_value(row.try_get::<Option<Vec<u8>>, _>(i).ok().flatten()),
        _ => pg_dec::<String>(row, i),
    };
    v.unwrap_or(serde_json::Value::Null)
}

fn sqlite_cell(row: &SqliteRow, i: usize) -> serde_json::Value {
    let raw = match row.try_get_raw(i) {
        Ok(v) => v,
        Err(_) => return serde_json::Value::Null,
    };
    if raw.is_null() {
        return serde_json::Value::Null;
    }
    let v = match raw.type_info().name() {
        "INTEGER" => sqlite_dec::<i64>(row, i).or_else(|| sqlite_dec::<bool>(row, i)),
        "REAL" => sqlite_dec::<f64>(row, i),
        "BLOB" => bytes_to_value(row.try_get::<Option<Vec<u8>>, _>(i).ok().flatten()),
        _ => sqlite_dec::<String>(row, i),
    };
    v.unwrap_or(serde_json::Value::Null)
}

// ================== 执行器 ==================

type RunOutput = (Vec<String>, Vec<Vec<serde_json::Value>>, u64);

async fn mysql_run(pool: &MySqlPool, sql: &str) -> Result<RunOutput, String> {
    if is_query_stmt(sql) {
        let rows = sqlx::query(sql).fetch_all(pool).await.map_err(|e| e.to_string())?;
        let columns = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();
        let mut data = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut row = Vec::with_capacity(r.columns().len());
            for i in 0..r.columns().len() {
                row.push(mysql_cell(r, i));
            }
            data.push(row);
        }
        Ok((columns, data, 0))
    } else {
        let res = sqlx::query(sql).execute(pool).await.map_err(|e| e.to_string())?;
        Ok((vec![], vec![], res.rows_affected()))
    }
}

async fn pg_run(pool: &PgPool, sql: &str) -> Result<RunOutput, String> {
    if is_query_stmt(sql) {
        let rows = sqlx::query(sql).fetch_all(pool).await.map_err(|e| e.to_string())?;
        let columns = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();
        let mut data = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut row = Vec::with_capacity(r.columns().len());
            for i in 0..r.columns().len() {
                row.push(pg_cell(r, i));
            }
            data.push(row);
        }
        Ok((columns, data, 0))
    } else {
        let res = sqlx::query(sql).execute(pool).await.map_err(|e| e.to_string())?;
        Ok((vec![], vec![], res.rows_affected()))
    }
}

async fn sqlite_run(pool: &SqlitePool, sql: &str) -> Result<RunOutput, String> {
    if is_query_stmt(sql) {
        let rows = sqlx::query(sql).fetch_all(pool).await.map_err(|e| e.to_string())?;
        let columns = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();
        let mut data = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut row = Vec::with_capacity(r.columns().len());
            for i in 0..r.columns().len() {
                row.push(sqlite_cell(r, i));
            }
            data.push(row);
        }
        Ok((columns, data, 0))
    } else {
        let res = sqlx::query(sql).execute(pool).await.map_err(|e| e.to_string())?;
        Ok((vec![], vec![], res.rows_affected()))
    }
}

// 统一分发执行
async fn run_dispatch(cfg: &DBConfig, sql: &str) -> Result<RunOutput, String> {
    match dispatch_db_type(cfg)? {
        "mysql" => mysql_run(&mysql_pool(cfg).await?, sql).await,
        "postgresql" => pg_run(&pg_pool(cfg).await?, sql).await,
        "sqlite" => sqlite_run(&sqlite_pool(cfg).await?, sql).await,
        _ => Err("不支持的数据库类型".to_string()),
    }
}

// ================== AI 命令 ==================

// 获取 AI 配置
#[command]
async fn get_ai_config() -> Result<AIConfig, String> {
    let data_dir = queries::get_data_dir();
    let config_path = data_dir.join("ai_config.json");
    
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("读取配置失败: {}", e))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("解析配置失败: {}", e))
    } else {
        // 返回空配置，前端会提示设置
        Ok(AIConfig {
            base_url: "".to_string(),
            api_key: "".to_string(),
            model: "".to_string(),
        })
    }
}

// 保存 AI 配置
#[command]
async fn save_ai_config(config: AIConfig) -> Result<(), String> {
    let data_dir = queries::get_data_dir();
    let config_path = data_dir.join("ai_config.json");
    
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("创建目录失败: {}", e))?;
    
    let content = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("序列化失败: {}", e))?;
    
    std::fs::write(&config_path, content)
        .map_err(|e| format!("保存配置失败: {}", e))?;
    
    Ok(())
}

// 调用 AI 服务
async fn call_ai_service(config: &AIConfig, prompt: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    
    let body = serde_json::json!({
        "message": prompt,
        "model": config.model
    });
    
    let response = client
        .post(format!("{}/chat/completions", config.base_url))
        .bearer_auth(&config.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;
    
    if response.status().is_success() {
        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("解析响应失败: {}", e))?;
        
        json["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "响应格式错误".to_string())
    } else {
        Err(format!("AI 服务错误: {}", response.status()))
    }
}

// AI 生成 SQL（自然语言 → SQL）
#[command]
async fn ai_generate_sql(natural_language: String, tables_info: Vec<TableInfo>, config: AIConfig) -> Result<String, String> {
    if config.base_url.is_empty() || config.api_key.is_empty() {
        return Err("请先在设置中配置 AI 参数".to_string());
    }
    
    let tables_detail = tables_info.iter()
        .map(|t| format!("表名: {}, 估计行数: {}, 大小: {} bytes", 
            t.name, t.row_estimate.unwrap_or(0), t.size_bytes.unwrap_or(0)))
        .collect::<Vec<_>>()
        .join("\n");
    
    let prompt = format!(
        "你是一个专业的数据库工程师。请根据以下表结构和自然语言描述，生成标准 SQL 语句。\n\n\
         表结构:\n{}\n\n\
         需求描述: {}\n\n\
         要求:\n\
         1. 只返回 SQL 语句，不要有额外解释\n\
         2. 使用标准 SQL 语法\n\
         3. 添加适当的注释说明\n\
         SQL:",
        tables_detail, natural_language
    );
    
    call_ai_service(&config, &prompt).await
}

// ================== 本地 Agent 客户端 ==================

// 调用本地 Agent
async fn call_agent_service(endpoint: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::new();
    
    let response = client
        .post(format!("http://localhost:8787{}", endpoint))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Agent 请求失败: {}", e))?;
    
    if response.status().is_success() {
        response.json().await.map_err(|e| format!("解析 Agent 响应失败: {}", e))
    } else {
        Err(format!("Agent 服务错误: {}", response.status()))
    }
}

// Agent: 查询建议（自然语言 → SQL）
#[command]
async fn agent_query_suggest(natural_language: String, config: DBConfig) -> Result<String, String> {
    // 获取表结构
    let tables = get_tables(config.clone()).await?;
    
    let body = serde_json::json!({
        "natural_language": natural_language,
        "context": {
            "connection_id": config.id,
            "database_type": config.db_type,
            "database_name": config.database,
            "tables": tables.iter().map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "row_estimate": t.row_estimate,
                    "size_bytes": t.size_bytes
                })
            }).collect::<Vec<_>>()
        }
    });
    
    let result = call_agent_service("/query/suggest", body).await?;
    
    result.get("sql")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Agent 响应格式错误".to_string())
}

// Agent: SQL 优化
#[command]
async fn agent_sql_optimize(sql: String, config: DBConfig) -> Result<String, String> {
    let body = serde_json::json!({
        "sql": sql,
        "context": {
            "connection_id": config.id,
            "database_type": config.db_type,
            "database_name": config.database,
        }
    });
    
    let result = call_agent_service("/sql/optimize", body).await?;
    
    result.get("optimized")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Agent 未返回优化结果".to_string())
}

// Agent: 结果分析
#[command]
async fn agent_results_analyze(sql: String, config: DBConfig) -> Result<String, String> {
    // 先执行查询
    let result = execute_query(sql.clone(), config.clone()).await?;
    
    let body = serde_json::json!({
        "sql": sql,
        "results": result.rows,
        "context": {
            "connection_id": config.id,
            "database_type": config.db_type,
            "database_name": config.database,
        }
    });
    
    let result = call_agent_service("/results/analyze", body).await?;
    
    result.get("analysis")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Agent 未返回分析结果".to_string())
}

// Agent: 错误诊断
#[command]
async fn agent_error_diagnose(error: String, sql: String, config: DBConfig) -> Result<String, String> {
    let body = serde_json::json!({
        "error": error,
        "sql": sql,
        "context": {
            "connection_id": config.id,
            "database_type": config.db_type,
            "database_name": config.database,
        }
    });
    
    let result = call_agent_service("/error/diagnose", body).await?;
    
    result.get("diagnosis")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Agent 未返回诊断结果".to_string())
}

// AI 解释查询结果
#[command]
async fn ai_explain_results(sql: String, results: Vec<serde_json::Value>, config: AIConfig) -> Result<String, String> {
    if config.base_url.is_empty() || config.api_key.is_empty() {
        return Err("请先在设置中配置 AI 参数".to_string());
    }
    
    let results_str = serde_json::to_string_pretty(&results)
        .unwrap_or_else(|_| "无法序列化结果".to_string());
    
    let prompt = format!(
        "你是一个数据分析专家。请分析以下 SQL 查询结果并提供洞察。\n\n\
         SQL: {}\n\n\
         查询结果:\n{}\n\n\
         请提供:\n\
         1. 结果摘要（行数、主要发现）\n\
         2. 异常值或值得注意的模式\n\
         3. 可能的业务含义\n\
         4. 进一步分析建议",
        sql, results_str
    );
    
    call_ai_service(&config, &prompt).await
}

// AI 优化 SQL
#[command]
async fn ai_optimize_sql(sql: String, table_schema: Vec<ColumnInfo>, config: AIConfig) -> Result<String, String> {
    if config.base_url.is_empty() || config.api_key.is_empty() {
        return Err("请先在设置中配置 AI 参数".to_string());
    }
    
    let schema_str = table_schema.iter()
        .map(|c| format!("- {} ({}), {}", c.name, c.data_type, 
            if c.is_primary { "主键" } else if !c.nullable { "NOT NULL" } else { "可空" }))
        .collect::<Vec<_>>()
        .join("\n");
    
    let prompt = format!(
        "你是一个 SQL 优化专家。请分析并优化以下 SQL 语句。\n\n\
         表结构:\n{}\n\n\
         原始 SQL: {}\n\n\
         请提供:\n\
         1. 优化后的 SQL 语句\n\
         2. 性能瓶颈分析\n\
         3. 索引建议\n\
         4. 最佳实践建议\n\n\
         优化后的 SQL:",
        schema_str, sql
    );
    
    call_ai_service(&config, &prompt).await
}

// AI 解释 SQL 执行逻辑
#[command]
async fn ai_explain_sql(sql: String, config: AIConfig) -> Result<String, String> {
    if config.base_url.is_empty() || config.api_key.is_empty() {
        return Err("请先在设置中配置 AI 参数".to_string());
    }
    
    let prompt = format!(
        "你是一个 SQL 教学专家。请详细解释以下 SQL 语句的执行逻辑。\n\n\
         SQL: {}\n\n\
         请提供:\n\
         1. 整体执行流程\n\
         2. 每个子句的作用\n\
         3. 数据流动过程\n\
         4. 潜在的性能问题",
        sql
    );
    
    call_ai_service(&config, &prompt).await
}

// AI 错误诊断
#[command]
async fn ai_diagnose_error(error_message: String, sql: String, config: AIConfig) -> Result<String, String> {
    if config.base_url.is_empty() || config.api_key.is_empty() {
        return Err("请先在设置中配置 AI 参数".to_string());
    }
    
    let prompt = format!(
        "你是一个数据库排错专家。请分析以下 SQL 错误。\n\n\
         错误信息: {}\n\n\
         执行的 SQL: {}\n\n\
         请提供:\n\
         1. 错误原因分析\n\
         2. 解决方案\n\
         3. 预防建议",
        error_message, sql
    );
    
    call_ai_service(&config, &prompt).await
}

// ================== Tauri 命令 ==================

// 测试数据库连接
#[command]
async fn test_connection(config: DBConfig) -> Result<bool, String> {
    dispatch_db_type(&config)?;
    run_dispatch(&config, "SELECT 1").await?;
    println!("Testing connection to {}...", config.name);
    Ok(true)
}

// 执行 SQL 查询
#[command]
async fn execute_query(sql: String, config: DBConfig) -> Result<QueryResult, String> {
    if sql.trim().is_empty() {
        return Err("SQL 语句为空".to_string());
    }
    let start = std::time::Instant::now();
    let (columns, rows, affected) = run_dispatch(&config, &sql).await?;
    let id = format!(
        "query_{}_{}",
        config.id,
        start.elapsed().as_nanos()
    );
    Ok(QueryResult {
        id,
        columns,
        rows,
        affected_rows: affected,
        execution_time_ms: start.elapsed().as_millis() as u64,
    })
}

// 获取数据库表列表
#[command]
async fn get_tables(config: DBConfig) -> Result<Vec<TableInfo>, String> {
    let mut out: Vec<TableInfo> = Vec::new();
    match dispatch_db_type(&config)? {
        "mysql" => {
            let pool = mysql_pool(&config).await?;
            let rows = sqlx::query(
                "SELECT table_name AS name, table_rows AS row_estimate, data_length AS size_bytes \
                 FROM information_schema.tables \
                 WHERE table_schema = ? AND table_type = 'BASE TABLE' ORDER BY table_name",
            )
            .bind(&config.database)
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
            for r in &rows {
                out.push(TableInfo {
                    name: r.try_get::<String, _>("name").unwrap_or_default(),
                    schema: Some(config.database.clone()),
                    row_estimate: r
                        .try_get::<Option<i64>, _>("row_estimate")
                        .ok()
                        .flatten()
                        .map(|v| v.max(0) as u64),
                    size_bytes: r
                        .try_get::<Option<i64>, _>("size_bytes")
                        .ok()
                        .flatten()
                        .map(|v| v.max(0) as u64),
                });
            }
        }
        "postgresql" => {
            let pool = pg_pool(&config).await?;
            let rows = sqlx::query(
                "SELECT c.relname AS name, n.nspname AS schema, \
                 GREATEST(c.reltuples::bigint, 0) AS row_estimate, \
                 pg_total_relation_size(c.oid) AS size_bytes \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind IN ('r','p') \
                   AND n.nspname NOT IN ('pg_catalog','information_schema') \
                   AND n.nspname NOT LIKE 'pg_toast%' \
                 ORDER BY n.nspname, c.relname",
            )
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
            for r in &rows {
                out.push(TableInfo {
                    name: r.try_get::<String, _>("name").unwrap_or_default(),
                    schema: r.try_get::<String, _>("schema").ok(),
                    row_estimate: r.try_get::<i64, _>("row_estimate").ok().map(|v| v as u64),
                    size_bytes: r.try_get::<i64, _>("size_bytes").ok().map(|v| v.max(0) as u64),
                });
            }
        }
        "sqlite" => {
            let pool = sqlite_pool(&config).await?;
            let rows = sqlx::query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
            for r in &rows {
                out.push(TableInfo {
                    name: r.try_get::<String, _>(0).unwrap_or_default(),
                    schema: None,
                    row_estimate: None,
                    size_bytes: None,
                });
            }
        }
        _ => {}
    }
    Ok(out)
}

// 获取表结构
#[command]
async fn get_table_structure(
    table_name: String,
    config: DBConfig,
) -> Result<Vec<ColumnInfo>, String> {
    let mut out: Vec<ColumnInfo> = Vec::new();
    match dispatch_db_type(&config)? {
        "mysql" => {
            let pool = mysql_pool(&config).await?;
            let rows = sqlx::query(
                "SELECT column_name, column_type, is_nullable, column_key, column_default \
                 FROM information_schema.columns \
                 WHERE table_schema = ? AND table_name = ? ORDER BY ordinal_position",
            )
            .bind(&config.database)
            .bind(&table_name)
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
            for r in &rows {
                out.push(ColumnInfo {
                    name: r.try_get::<String, _>(0).unwrap_or_default(),
                    data_type: r.try_get::<String, _>(1).unwrap_or_default(),
                    nullable: r.try_get::<String, _>(2).unwrap_or_default() == "YES",
                    is_primary: r.try_get::<String, _>(3).unwrap_or_default() == "PRI",
                    default_value: r.try_get::<Option<String>, _>(4).ok().flatten(),
                });
            }
        }
        "postgresql" => {
            let pool = pg_pool(&config).await?;
            let rows = sqlx::query(
                "SELECT c.column_name, c.data_type, c.is_nullable, c.column_default, \
                 COALESCE((SELECT true FROM information_schema.table_constraints tc \
                   JOIN information_schema.key_column_usage kcu \
                     ON tc.constraint_name = kcu.constraint_name \
                    AND tc.table_schema = kcu.table_schema \
                  WHERE tc.constraint_type = 'PRIMARY KEY' \
                    AND tc.table_schema = c.table_schema \
                    AND tc.table_name = c.table_name \
                    AND kcu.column_name = c.column_name), false) AS is_primary \
                 FROM information_schema.columns c \
                 WHERE c.table_schema = current_schema() AND c.table_name = $1 \
                 ORDER BY c.ordinal_position",
            )
            .bind(&table_name)
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
            for r in &rows {
                out.push(ColumnInfo {
                    name: r.try_get::<String, _>(0).unwrap_or_default(),
                    data_type: r.try_get::<String, _>(1).unwrap_or_default(),
                    nullable: r.try_get::<String, _>(2).unwrap_or_default() == "YES",
                    is_primary: r.try_get::<bool, _>(4).unwrap_or(false),
                    default_value: r.try_get::<Option<String>, _>(3).ok().flatten(),
                });
            }
        }
        "sqlite" => {
            let pool = sqlite_pool(&config).await?;
            let sql = format!(
                "PRAGMA table_info('{}')",
                table_name.replace('\'', "''")
            );
            let rows = sqlx::query(&sql)
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
            for r in &rows {
                out.push(ColumnInfo {
                    name: r.try_get::<String, _>(1).unwrap_or_default(),
                    data_type: r.try_get::<String, _>(2).unwrap_or_default(),
                    nullable: r.try_get::<i64, _>(3).unwrap_or(1) == 0,
                    is_primary: r.try_get::<i64, _>(5).unwrap_or(0) > 0,
                    default_value: r.try_get::<Option<String>, _>(4).ok().flatten(),
                });
            }
        }
        _ => {}
    }
    Ok(out)
}

// 执行多条 SQL（批量）
#[command]
async fn execute_batch(
    queries: Vec<String>,
    config: DBConfig,
) -> Result<Vec<QueryResult>, String> {
    let mut out = Vec::new();
    for q in queries {
        if q.trim().is_empty() {
            continue;
        }
        let start = std::time::Instant::now();
        let (columns, rows, affected) = run_dispatch(&config, &q).await?;
        let id = format!(
            "query_{}_{}",
            config.id,
            start.elapsed().as_nanos()
        );
        out.push(QueryResult {
            id,
            columns,
            rows,
            affected_rows: affected,
            execution_time_ms: start.elapsed().as_millis() as u64,
        });
    }
    Ok(out)
}

// 格式化 SQL（简单实现：关键字大写、统一换行缩进）
#[command]
async fn format_sql(sql: String) -> Result<String, String> {
    let keywords = [
        "SELECT", "FROM", "WHERE", "AND", "OR", "ORDER BY", "GROUP BY", "HAVING", "LIMIT",
        "JOIN", "LEFT JOIN", "RIGHT JOIN", "INNER JOIN", "OUTER JOIN", "UNION", "INSERT INTO",
        "VALUES", "UPDATE", "SET", "DELETE FROM",
    ];
    let mut result = sql;
    for kw in keywords {
        result = regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(kw)))
            .map_err(|e| e.to_string())?
            .replace_all(&result, kw)
            .to_string();
    }
    for kw in ["FROM", "WHERE", "ORDER BY", "GROUP BY", "HAVING", "LIMIT", "UNION"] {
        result = regex::Regex::new(&format!(r"\b{}\b", regex::escape(kw)))
            .map_err(|e| e.to_string())?
            .replace_all(&result, &format!("\n{}", kw))
            .to_string();
    }
    Ok(result)
}

// 导出连接配置
#[command]
async fn export_connections(
    connections: Vec<ExportedConnection>,
) -> Result<String, String> {
    let export = ConnectionExport {
        version: 1,
        exported_at: chrono::Utc::now().timestamp(),
        connections,
    };
    serde_json::to_string_pretty(&export).map_err(|e| e.to_string())
}

// 导入连接配置
#[command]
async fn import_connections(json: String) -> Result<Vec<ExportedConnection>, String> {
    let export: ConnectionExport =
        serde_json::from_str(&json).map_err(|e| format!("解析失败: {}", e))?;
    Ok(export.connections)
}

// 连接配置落盘
#[command]
async fn save_connections(connections: Vec<ConnectionRecord>) -> Result<(), String> {
    queries::save_connections(&connections).await
}

#[command]
async fn load_connections() -> Result<Vec<ConnectionRecord>, String> {
    queries::load_connections().await
}

// 查询历史记录
#[command]
async fn save_query_history(history: Vec<QueryHistoryItem>) -> Result<(), String> {
    queries::save_history(&history).await
}

#[command]
async fn load_query_history() -> Result<Vec<QueryHistoryItem>, String> {
    queries::load_history().await
}

#[command]
async fn clear_query_history() -> Result<(), String> {
    queries::clear_history().await
}

// 保存常用查询
#[command]
async fn save_query(item: SavedQuery) -> Result<(), String> {
    queries::save_saved_query(item).await
}

#[command]
async fn load_saved_queries() -> Result<Vec<SavedQuery>, String> {
    queries::load_saved_queries().await
}

#[command]
async fn delete_saved_query(id: String) -> Result<(), String> {
    queries::delete_saved_query(&id).await
}

// ================== Tauri 启动 ==================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            test_connection,
            execute_query,
            get_tables,
            get_table_structure,
            execute_batch,
            format_sql,
            export_connections,
            import_connections,
            save_connections,
            load_connections,
            save_query_history,
            load_query_history,
            clear_query_history,
            save_query,
            load_saved_queries,
            delete_saved_query,
            get_ai_config,
            save_ai_config,
            ai_generate_sql,
            ai_explain_results,
            ai_optimize_sql,
            ai_explain_sql,
            ai_diagnose_error,
            agent_query_suggest,
            agent_sql_optimize,
            agent_results_analyze,
            agent_error_diagnose,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ================== 真实测试（只做 SELECT，不碰写/删） ==================

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(db_type: &str, host: &str, port: u16, db: &str) -> DBConfig {
        DBConfig {
            id: "t".into(),
            name: "test".into(),
            db_type: db_type.into(),
            host: host.into(),
            port,
            username: "root".into(),
            password: String::new(),
            database: db.into(),
        }
    }

    #[tokio::test]
    async fn sqlite_select_only() {
        let c = cfg("sqlite", "", 0, ":memory:");
        test_connection(c.clone()).await.expect("sqlite 连接失败");
        let r = execute_query("SELECT 1 AS one, 'x' AS s, NULL AS z".into(), c.clone())
            .await
            .expect("sqlite 查询失败");
        assert_eq!(r.rows[0][0], serde_json::json!(1));
        assert_eq!(r.rows[0][1], serde_json::json!("x"));
        assert_eq!(r.rows[0][2], serde_json::Value::Null);
        // 空库表列表为空、不存在的表结构为空
        assert!(get_tables(c.clone()).await.unwrap().is_empty());
        assert!(get_table_structure("no_such_table".into(), c)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn mysql_select_only() {
        let c = cfg("mysql", "127.0.0.1", 3306, "mysql");
        if let Err(e) = test_connection(c.clone()).await {
            eprintln!("[skip] 本机 MySQL 不可用: {e}");
            return;
        }
        let r = execute_query(
            "SELECT VERSION() AS v, 123 AS n, NULL AS z".into(),
            c.clone(),
        )
        .await
        .expect("mysql 查询失败");
        assert_eq!(r.rows[0][1], serde_json::json!(123));
        assert_eq!(r.rows[0][2], serde_json::Value::Null);
        let tables = get_tables(c.clone()).await.unwrap();
        assert!(!tables.is_empty(), "mysql 系统库应有表");
        let cols = get_table_structure(tables[0].name.clone(), c.clone())
            .await
            .unwrap();
        assert!(!cols.is_empty(), "应能读到表结构");
        // host 误带端口时容错
        let c2 = cfg("mysql", "127.0.0.1:3306", 0, "mysql");
        test_connection(c2).await.expect("host:port 容错失败");
        // 不存在的库应快速失败并透出真实错误
        let bad = cfg("mysql", "127.0.0.1", 3306, "no_such_db_xx");
        let err = test_connection(bad).await.expect_err("应报错");
        eprintln!("[ok] 错误透传示例: {err}");
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn connections_persist_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("zbiz-db-test-{}", std::process::id()));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);
        let records = vec![ConnectionRecord {
            id: "1".into(),
            name: "本地 MySQL".into(),
            db_type: "mysql".into(),
            host: "127.0.0.1".into(),
            port: 3306,
            username: "root".into(),
            password: String::new(),
            database: "mysql".into(),
        }];
        queries::save_connections(&records).await.unwrap();
        let loaded = queries::load_connections().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].db_type, "mysql");
        assert_eq!(loaded[0].port, 3306);
    }
}
