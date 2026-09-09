use serde::{Deserialize, Serialize};
use tauri::command;

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
pub struct ConnectionExport {
    pub version: u32,
    pub exported_at: i64,
    pub connections: Vec<ExportedConnection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedConnection {
    pub name: String,
    pub db_type: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub database: String,
    // 密码不导出（安全考虑）
}

// ================== Tauri 命令 ==================

// 测试数据库连接
#[command]
pub async fn test_connection(config: DBConfig) -> Result<bool, String> {
    println!("Testing connection to {}...", config.name);
    Ok(true)
}

// 执行 SQL 查询
#[command]
pub async fn execute_query(sql: String, config: DBConfig) -> Result<QueryResult, String> {
    println!("Executing query: {}", sql);
    let start = std::time::Instant::now();

    // TODO: 真实的 SQL 执行（使用 sqlx）
    let result = QueryResult {
        columns: vec![
            "id".to_string(),
            "name".to_string(),
            "email".to_string(),
            "created_at".to_string(),
        ],
        rows: vec![
            vec![
                serde_json::Value::Number(1.into()),
                serde_json::Value::String("张三".to_string()),
                serde_json::Value::String("zhangsan@example.com".to_string()),
                serde_json::Value::String("2024-01-15 10:30:00".to_string()),
            ],
            vec![
                serde_json::Value::Number(2.into()),
                serde_json::Value::String("李四".to_string()),
                serde_json::Value::String("lisi@example.com".to_string()),
                serde_json::Value::String("2024-01-16 14:22:00".to_string()),
            ],
            vec![
                serde_json::Value::Number(3.into()),
                serde_json::Value::String("王五".to_string()),
                serde_json::Value::String("wangwu@example.com".to_string()),
                serde_json::Value::String("2024-01-17 09:15:00".to_string()),
            ],
        ],
        affected_rows: 0,
        execution_time_ms: start.elapsed().as_millis(),
    };

    Ok(result)
}

// 获取数据库表列表
#[command]
pub async fn get_tables(config: DBConfig) -> Result<Vec<TableInfo>, String> {
    println!("Getting tables for {}...", config.name);
    Ok(vec![
        TableInfo {
            name: "users".to_string(),
            schema: Some("public".to_string()),
            row_estimate: Some(1234),
            size_bytes: Some(524288),
        },
        TableInfo {
            name: "orders".to_string(),
            schema: Some("public".to_string()),
            row_estimate: Some(5678),
            size_bytes: Some(2097152),
        },
        TableInfo {
            name: "products".to_string(),
            schema: Some("public".to_string()),
            row_estimate: Some(890),
            size_bytes: Some(1048576),
        },
        TableInfo {
            name: "categories".to_string(),
            schema: Some("public".to_string()),
            row_estimate: Some(45),
            size_bytes: Some(16384),
        },
    ])
}

// 获取表结构
#[command]
pub async fn get_table_structure(
    table_name: String,
    config: DBConfig,
) -> Result<Vec<ColumnInfo>, String> {
    println!("Getting structure for table {}...", table_name);
    Ok(vec![
        ColumnInfo {
            name: "id".to_string(),
            data_type: "INTEGER".to_string(),
            nullable: false,
            is_primary: true,
            default_value: Some("AUTO_INCREMENT".to_string()),
        },
        ColumnInfo {
            name: "name".to_string(),
            data_type: "VARCHAR(255)".to_string(),
            nullable: false,
            is_primary: false,
            default_value: None,
        },
        ColumnInfo {
            name: "email".to_string(),
            data_type: "VARCHAR(255)".to_string(),
            nullable: true,
            is_primary: false,
            default_value: None,
        },
        ColumnInfo {
            name: "created_at".to_string(),
            data_type: "TIMESTAMP".to_string(),
            nullable: false,
            is_primary: false,
            default_value: Some("CURRENT_TIMESTAMP".to_string()),
        },
    ])
}

// 执行多条 SQL（批量）
#[command]
pub async fn execute_batch(
    queries: Vec<String>,
    config: DBConfig,
) -> Result<Vec<QueryResult>, String> {
    let mut results = Vec::new();
    for q in queries {
        let trimmed = q.trim();
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }
        let result = execute_query(q.clone(), config.clone()).await?;
        results.push(result);
    }
    Ok(results)
}

// 格式化 SQL（使用外部 crate 或简单实现）
#[command]
pub async fn format_sql(sql: String) -> Result<String, String> {
    // 简单格式化：关键字大写、统一缩进
    let keywords = [
        "SELECT", "FROM", "WHERE", "AND", "OR", "ORDER BY", "GROUP BY",
        "HAVING", "LIMIT", "OFFSET", "INSERT", "INTO", "VALUES",
        "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "DROP", "ALTER",
        "ADD", "COLUMN", "PRIMARY", "KEY", "FOREIGN", "REFERENCES",
        "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "ON", "AS", "AND",
        "OR", "NOT", "NULL", "IS", "IN", "EXISTS", "BETWEEN", "LIKE",
    ];

    let mut formatted = sql.clone();
    for kw in &keywords {
        let re = regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(kw))).unwrap();
        formatted = re.replace_all(&formatted, *kw).to_string();
    }
    Ok(formatted)
}

// 导出连接配置
#[command]
pub async fn export_connections(
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
pub async fn import_connections(json: String) -> Result<Vec<ExportedConnection>, String> {
    let export: ConnectionExport =
        serde_json::from_str(&json).map_err(|e| format!("解析失败: {}", e))?;
    Ok(export.connections)
}

// 查询历史记录
#[command]
pub async fn save_query_history(history: Vec<QueryHistoryItem>) -> Result<(), String> {
    queries::save_history(&history).await
}

#[command]
pub async fn load_query_history() -> Result<Vec<QueryHistoryItem>, String> {
    queries::load_history().await
}

#[command]
pub async fn clear_query_history() -> Result<(), String> {
    queries::clear_history().await
}

// 保存常用查询
#[command]
pub async fn save_query(item: SavedQuery) -> Result<(), String> {
    queries::save_saved_query(item).await
}

#[command]
pub async fn load_saved_queries() -> Result<Vec<SavedQuery>, String> {
    queries::load_saved_queries().await
}

#[command]
pub async fn delete_saved_query(id: String) -> Result<(), String> {
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
            save_query_history,
            load_query_history,
            clear_query_history,
            save_query,
            load_saved_queries,
            delete_saved_query,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}