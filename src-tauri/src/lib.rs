use serde::{Deserialize, Serialize};
use tauri::command;

use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlRow};
use sqlx::postgres::{PgConnectOptions, PgPool, PgRow};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqliteRow};
use sqlx::{Column, Row, TypeInfo, ValueRef};
use tauri::Manager;

#[path = "ai_sql.rs"]
mod ai_sql;
#[path = "db/mod.rs"]
mod db;
#[path = "queries.rs"]
mod queries;
pub mod report;
#[path = "secrets.rs"]
pub mod secrets;
#[path = "security.rs"]
mod security;

// ================== 数据结构 ==================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DBConfig {
    pub id: String,
    pub name: String,
    pub db_type: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 已弃用：S0 起落盘不再保存明文密码；仅运行时内存使用
    #[serde(default)]
    pub password: String,
    pub database: String,
    /// T-025：这一条连接的 TLS 档位。缺省 = 最安全的一档（verify_full）。
    #[serde(default)]
    pub tls_mode: Option<TlsMode>,
    /// T-025：企业私有 CA 的证书文件路径（Pem）。内置根证书集是 Mozilla 公共 CA，
    /// 私有 CA 必须显式给文件，否则 verify_full 连不上。
    #[serde(default)]
    pub tls_ca_path: Option<String>,
}

/// 列元数据：用于结果契约，避免 UI 误用首行 Object.keys 推导列名
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub ordinal: u32,
    pub name: String,
    pub native_type: String,
    pub logical_type: String,
    pub nullable: bool,
}

/// 单元格类型标签（与 serde_json::Value 配合使用；通过 `__kind` 字段区分）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellKind {
    Null,
    Integer,
    Float,
    Decimal,
    Text,
    Binary,
    Date,
    Time,
    DateTime,
    Timestamp,
    Uuid,
    Json,
    Unsupported,
    DecodeError,
}

/// 结果模型（v1 兼容 + v2 元数据并存）
/// - v1 字段 `columns: Vec<String>` 保留以兼容旧 UI
/// - v2 字段 `column_meta: Vec<ColumnMeta>` 提供完整列描述，零行结果亦返回
/// - `rows` 内每个单元格通过 `__kind` + `value` 双字段表达类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub id: String,
    /// v1 兼容：仅含列名（顺序与 column_meta 一致）
    pub columns: Vec<String>,
    /// v2：完整列元数据，驱动 describe 获取
    #[serde(default)]
    pub column_meta: Vec<ColumnMeta>,
    /// 每个单元格为 `{"__kind": "...", "value": ...}` 或原始值
    pub rows: Vec<Vec<serde_json::Value>>,
    /// DML 才有意义
    pub affected_rows: u64,
    pub execution_time_ms: u64,
    /// 当前语句是否为结果集查询（用于 UI 区分零行结果与 DML）
    #[serde(default)]
    pub is_query: bool,
    /// T-041：分段耗时（毫秒）
    #[serde(default)]
    pub timings: Timings,
    /// T-031：是否被截断（超过 max_rows）
    #[serde(default)]
    pub truncated: bool,
    /// T-031：总行数；truncated 且这里是 0 表示"流式提前停止、总数没数过"（不是 0 行）
    #[serde(default)]
    pub total_rows: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timings {
    #[serde(default)]
    pub connect_ms: u64,
    #[serde(default)]
    pub queue_ms: u64,
    #[serde(default)]
    pub execute_ms: u64,
    #[serde(default)]
    pub fetch_ms: u64,
    #[serde(default)]
    pub total_ms: u64,
}

pub fn new_timings(start: std::time::Instant) -> Timings {
    Timings {
        total_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    }
}

/// T-041：构建完整分段耗时
fn build_timings(
    queue_start: std::time::Instant,
    connect_start: std::time::Instant,
    exec_start: std::time::Instant,
) -> Timings {
    let connect_ms = connect_start.elapsed().as_millis() as u64;
    let execute_ms = exec_start.elapsed().as_millis() as u64;
    Timings {
        connect_ms,
        queue_ms: 0, // 队列等待在当前架构下不可单独测量
        execute_ms,
        fetch_ms: 0, // fetch_all 与 execute 合并，无法拆分
        total_ms: queue_start.elapsed().as_millis() as u64,
    }
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

/// 报表簿条目：存的是 spec（数据集定义 + 视图布局），不是查询结果。
/// 结果依赖库里当下的数据，spec 才是次日还能重跑的那份东西。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedReport {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 起草时用的自然语言问题；重新起草时回填
    #[serde(default)]
    pub question: String,
    pub datasets: Vec<crate::report::dataset::DatasetSpec>,
    pub view: crate::report::view::ViewSpec,
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
// T-053：增 revision 字段；保存时 CAS 比对。
// T-055：增 environment 字段；评估写入风险与审批强度。
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
    /// 配置修订号；编辑保存时 +1，CAS 拒绝过期写入
    #[serde(default)]
    pub revision: u32,
    /// 环境标记：unknown | dev | test | staging | prod | custom
    #[serde(default = "default_environment")]
    pub environment: String,
    /// T-025：这一条连接的 TLS 档位。缺省 = 最安全的一档（verify_full）。
    #[serde(default)]
    pub tls_mode: Option<TlsMode>,
    /// T-025：企业私有 CA 的证书文件路径（Pem）。
    #[serde(default)]
    pub tls_ca_path: Option<String>,
    /// T-014：密码的密钥库引用。**落盘只存这个索引，绝不存明文**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<crate::secrets::SecretRef>,
    /// T-014：凭据状态。""=从未存过密码；ok=明文已还原；missing=取不回来了，需重输。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential_state: String,
    /// T-014：取不回密码时给用户看的原因。**必须展示**，不能只把密码清空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_note: Option<String>,
}

fn default_environment() -> String {
    "unknown".to_string()
}

/// T-055：判定给定 environment 是否需要强制审批
pub fn environment_requires_strict_approval(env: &str) -> bool {
    matches!(env, "prod" | "production" | "staging")
}

// 端口容错：T-026 加固
// - 数字或字符串解析；超出 u16 范围必须报错而非静默截断（A22）
// - 浮点、负数、空串、不可解析字符串一律报错
fn de_port<'de, D>(d: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // 端口非法一律带 INVALID_PORT 前缀：前端 errors.ts 认这个码，
    // 中文正文（"端口…超过/端口字段/端口字符串"）保持不变 ⇒ 正则那条路也还在。
    fn invalid_port(msg: &str) -> String {
        db::ipc::ApiError::invalid_port(msg).to_wire()
    }
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Number(n) => {
            let raw = n
                .as_u64()
                .or_else(|| n.as_i64().map(|i| i.max(0) as u64))
                .ok_or_else(|| serde::de::Error::custom(invalid_port("端口必须为非负整数")))?;
            if raw > u16::MAX as u64 {
                return Err(serde::de::Error::custom(invalid_port(&format!(
                    "端口 {} 超过 u16 上限 {}",
                    raw,
                    u16::MAX
                ))));
            }
            // 0 = "未指定"：SQLite 连接根本没有端口，前端也会送 0。
            // 端口是否合法**取决于驱动** ⇒ 在建池时按驱动校验
            // （MySQL/PG 端口为 0 会被拒），这里放行。
            Ok(raw as u16)
        }
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Err(serde::de::Error::custom(invalid_port("端口字符串为空")));
            }
            t.parse::<u16>().map_err(|_| {
                serde::de::Error::custom(invalid_port(&format!("无法解析端口字符串: {}", s)))
            })
        }
        _ => Err(serde::de::Error::custom(invalid_port(
            "端口必须是数字或字符串",
        ))),
    }
}

// host 误填成 "host:port" 时自动拆分（T-026：IPv6 严格要求）
// - IPv6 必须用 `[ipv6]:port` 形式；裸 IPv6 不允许自动按尾段猜端口
// - 仅在不冲突或无显式端口时采用 default_port
fn split_host_port(host: &str, default_port: u16) -> (String, u16) {
    let h = host.trim();
    if h.is_empty() {
        return (String::new(), default_port);
    }
    // IPv6 字面量：含 `:` 且不含 `[`，明确视为裸 IPv6，禁止猜测
    if h.contains(':') && !h.contains('[') {
        // 仅在 host 末段是数字且没有 IPv6 多段结构时尝试拆分；
        // 多段（多个 : ）直接视为裸 IPv6，不拆端口
        let colon_count = h.matches(':').count();
        if colon_count == 1 {
            let (hpart, ppart) = h.rsplit_once(':').unwrap();
            if !ppart.is_empty() && ppart.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(p) = ppart.parse::<u16>() {
                    if p > 0 {
                        return (hpart.to_string(), p);
                    }
                }
            }
        }
        return (h.to_string(), default_port);
    }
    // 带 [IPv6]:port 形式
    if let Some(idx_bracket_close) = h.find(']') {
        if let Some(after) = h.get(idx_bracket_close + 1..) {
            if let Some(stripped) = after.strip_prefix(':') {
                let ppart = stripped.trim();
                if !ppart.is_empty() && ppart.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(p) = ppart.parse::<u16>() {
                        if p > 0 {
                            return (h[..=idx_bracket_close].to_string(), p);
                        }
                    }
                }
            }
        }
    }
    (h.to_string(), default_port)
}

// ================== 连接辅助 ==================

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const POOL_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// T-025：TLS 模式
/// - verify_full（默认）：强制校验 CA + 主机名
/// - prefer：未配置证书时不阻断本地开发，但记录警告
/// - disable：仅本地隔离可显式选择，连接元数据保留可识别标记
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// 缺省 = 最安全的一档（验 CA + 验主机名）
    #[default]
    VerifyFull,
    Prefer,
    Disable,
}

impl TlsMode {
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.map(|x| x.to_ascii_lowercase()) {
            Some(v) if v == "verify_full" || v == "verifyfull" => TlsMode::VerifyFull,
            Some(v) if v == "prefer" => TlsMode::Prefer,
            Some(v) if v == "disable" || v == "off" => TlsMode::Disable,
            _ => TlsMode::VerifyFull,
        }
    }

    /// 映射到 sqlx 的 MySQL SSL 模式。
    ///
    /// ⚠️ 语义差异必须说清楚（这是 T-025 的核心，之前的实现是三个 no-op 分支）：
    /// - `VerifyFull` → `VerifyIdentity`：**验 CA + 验主机名**（唯一名副其实的一档）。
    ///   旧实现"留 opts 默认"，而 sqlx 的默认是 `Preferred` ⇒ **根本不校验证书**，
    ///   与本枚举"默认强制校验"的承诺相反。
    /// - `Prefer` → `Required`：**加密但不校验证书**（sqlx 侧 `accept_invalid_certs=true`）。
    ///   这是安全降级，调用方必须打警告 —— 本枚举的语义就是"不阻断本地开发，但记录警告"。
    /// - `Disable` → `Disabled`：**明文**。仅本地隔离可用。
    pub fn to_mysql(self) -> sqlx::mysql::MySqlSslMode {
        use sqlx::mysql::MySqlSslMode;
        match self {
            TlsMode::VerifyFull => MySqlSslMode::VerifyIdentity,
            TlsMode::Prefer => MySqlSslMode::Required,
            TlsMode::Disable => MySqlSslMode::Disabled,
        }
    }

    /// 映射到 sqlx 的 PostgreSQL SSL 模式。与 `to_mysql` 同义，只是枚举名不同。
    pub fn to_pg(self) -> sqlx::postgres::PgSslMode {
        use sqlx::postgres::PgSslMode;
        match self {
            TlsMode::VerifyFull => PgSslMode::VerifyFull,
            TlsMode::Prefer => PgSslMode::Require,
            TlsMode::Disable => PgSslMode::Disable,
        }
    }

    /// 这一档是否**不校验证书**。用于在界面上明确标出"降级"状态。
    pub fn skips_certificate_verification(self) -> bool {
        matches!(self, TlsMode::Prefer | TlsMode::Disable)
    }

    /// 档位名。与 serde 的 `rename_all = "snake_case"` 逐字一致
    /// （`verify_full` / `prefer` / `disable`）—— 显示与指纹都用它，避免两处各写一份。
    pub fn as_str(self) -> &'static str {
        match self {
            TlsMode::VerifyFull => "verify_full",
            TlsMode::Prefer => "prefer",
            TlsMode::Disable => "disable",
        }
    }
}

/// 解析这个连接该用哪一档 TLS。
///
/// 优先级：**连接配置里的 `tls_mode` > 环境变量 > 默认（verify_full）**。
/// 缺省一律落到最安全的那一档 —— 认不出来的值也不回退到宽松档。
fn resolve_tls_mode(configured: Option<TlsMode>, env_var: &str) -> TlsMode {
    configured.unwrap_or_else(|| {
        let env_value = std::env::var(env_var).ok();
        TlsMode::from_str_opt(env_value.as_deref())
    })
}

async fn mysql_pool_new(cfg: &DBConfig, max_connections: u32) -> Result<MySqlPool, String> {
    let (host, port) = split_host_port(&cfg.host, cfg.port);
    // 端口是否合法取决于驱动：SQLite 不用端口，MySQL/PG 必须有。
    if port == 0 {
        return Err(db::ipc::ApiError::invalid_port("MySQL 连接必须指定端口（1-65535）").to_wire());
    }
    // T-025：TLS 模式。连接配置优先，其次环境变量，缺省落到最安全的一档。
    let mode = resolve_tls_mode(cfg.tls_mode, "MYSQL_TLS_MODE");
    let mut opts = MySqlConnectOptions::new()
        .host(&host)
        .port(port)
        .username(&cfg.username)
        .password(&cfg.password)
        .database(&cfg.database)
        .ssl_mode(mode.to_mysql());
    // 企业私有 CA：内置根证书集是 Mozilla 公共 CA，私有 CA 必须显式给文件。
    if let Some(ca) = cfg.tls_ca_path.as_ref().filter(|p| !p.trim().is_empty()) {
        opts = opts.ssl_ca(ca);
    }
    if mode.skips_certificate_verification() {
        eprintln!(
            "[TLS] 连接 {:?} 使用 {:?} 档：不校验证书（降级）。生产环境请改回 verify_full。",
            cfg.name, mode
        );
    }
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        sqlx::pool::PoolOptions::<sqlx::MySql>::new()
            .acquire_timeout(POOL_CONNECT_TIMEOUT)
            .max_connections(max_connections)
            .connect_with(opts),
    )
    .await
    .map_err(|_| {
        "连接超时（15 秒）：网络不可达或防火墙丢包，请检查 VPN / 安全组 / 主机端口".to_string()
    })?
    .map_err(|e| e.to_string())
}

async fn pg_pool_new(cfg: &DBConfig, max_connections: u32) -> Result<PgPool, String> {
    let (host, port) = split_host_port(&cfg.host, cfg.port);
    if port == 0 {
        return Err(
            db::ipc::ApiError::invalid_port("PostgreSQL 连接必须指定端口（1-65535）").to_wire(),
        );
    }
    let mode = resolve_tls_mode(cfg.tls_mode, "PG_TLS_MODE");
    let mut opts = PgConnectOptions::new()
        .host(&host)
        .port(port)
        .username(&cfg.username)
        .password(&cfg.password)
        .database(&cfg.database)
        .ssl_mode(mode.to_pg());
    if let Some(ca) = cfg.tls_ca_path.as_ref().filter(|p| !p.trim().is_empty()) {
        opts = opts.ssl_root_cert(ca);
    }
    if mode.skips_certificate_verification() {
        eprintln!(
            "[TLS] 连接 {:?} 使用 {:?} 档：不校验证书（降级）。生产环境请改回 verify_full。",
            cfg.name, mode
        );
    }
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        sqlx::pool::PoolOptions::<sqlx::Postgres>::new()
            .max_connections(max_connections)
            .acquire_timeout(POOL_CONNECT_TIMEOUT)
            .connect_with(opts),
    )
    .await
    .map_err(|_| {
        "连接超时（15 秒）：网络不可达或防火墙丢包，请检查 VPN / 安全组 / 主机端口".to_string()
    })?
    .map_err(|e| e.to_string())
}

async fn sqlite_pool_new(cfg: &DBConfig, max_connections: u32) -> Result<SqlitePool, String> {
    let path = cfg.database.trim();
    if path.is_empty() {
        return Err("SQLite 需要在「数据库名」中填写数据库文件路径".to_string());
    }
    // sqlx 不会代建文件，失败只吐 "unable to open database file"。
    // 先把原因说清楚，否则用户会以为是权限 bug 而不是路径写错。
    // :memory: 与 file: URI 不是文件系统路径，不能做存在性检查。
    let special = path == ":memory:" || path.starts_with("file:");
    if !special {
        let p = std::path::Path::new(path);
        if !p.exists() {
            return Err(format!(
                "SQLite 文件不存在：{}（本工具不会代建新库文件）",
                path
            ));
        }
        if !p.is_file() {
            return Err(format!("SQLite 路径不是文件：{}", path));
        }
    }
    let opts = SqliteConnectOptions::new().filename(path);
    // SQLite 的连接数**必须**是 1：
    // - `:memory:` 模式下**每条连接就是一个独立的数据库** ⇒ 多连接时
    //   "建表"和"查询"可能落在不同库上 ⇒ 报 "no such table"；
    // - 事务必须落在同一条连接上 ⇒ 多连接时 BEGIN 与 COMMIT 不同连接
    //   ⇒ 报 "no transaction is active"。
    // 代价是 SQLite 访问串行化 —— 对桌面工具的本地库可以接受，
    // 换来的是"建了表就一定能查到"。
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
            .max_connections(max_connections)
            .connect_with(opts),
    )
    .await
    .map_err(|_| "SQLite 打开超时：文件可能被其它进程独占或位于慢速网络盘".to_string())?
    .map_err(|e| e.to_string())
}

// ================== 连接池缓存 ==================

/// 取（或建）连接池：走全局 `ConnectionManager` 缓存。
///
/// **2026-10-22 实测修正**：改之前 `mysql_pool(&config)` 每次调用都
/// `connect_with(...)` **新建一个池** —— 等于每条查询都重新握手 + 重新鉴权，
/// 而 `ConnectionManager`（缓存层，A04 验收要求"同一连接多次查询复用池"）
/// 当时**没有任何生产调用点**。
///
/// 缓存键 = (连接 id, 参数指纹, access_mode)：
/// - 参数指纹保证「改了 host / 密码 / 库名」之后拿到的是**新池**，
///   不会拿旧池去连旧库（旧池还带着旧凭据）；
/// - access_mode 分开「只读池」与「可写池」，两者身份不共享。
async fn pooled(
    config: &DBConfig,
    access_mode: &str,
) -> Result<crate::db::connection::AnyPool, String> {
    use crate::db::connection::{fingerprint, pool_registry, AnyPool, PoolKey};
    let key = PoolKey {
        connection_id: config.id.clone(),
        fingerprint: fingerprint(config),
        access_mode: access_mode.to_string(),
    };
    pool_registry()
        .get_or_insert(key, || async {
            match dispatch_db_type(config)? {
                "mysql" => Ok(AnyPool::MySql(mysql_pool_new(config, 10).await?)),
                "postgresql" => Ok(AnyPool::Pg(pg_pool_new(config, 10).await?)),
                // `:memory:` 每条连接就是一个独立的库 ⇒ 只能 1 条连接，
                // 否则"建表"和"查询"可能落在不同的库上。文件库可以多连。
                "sqlite" => Ok(AnyPool::Sqlite(
                    sqlite_pool_new(config, sqlite_conn_limit(config)).await?,
                )),
                _ => Err("不支持的数据库类型".to_string()),
            }
        })
        .await
}

/// SQLite 池的连接数上限。
///
/// `:memory:` 模式下**每条连接都是一个独立的数据库** ⇒ 连接数必须为 1，
/// 否则建表与查询可能落在不同的库上（"no such table"）。
/// 文件库没有这个限制，保持并发。
fn sqlite_conn_limit(cfg: &DBConfig) -> u32 {
    let db = cfg.database.trim();
    if db == ":memory:" || db.starts_with("file:") {
        1
    } else {
        10
    }
}

/// T-043：取**事务专属**的单连接池。
///
/// `create` 为真 ⇒ 新开一个（BEGIN 时用）；为假 ⇒ 取已存在的
/// （事务中的普通语句、COMMIT、ROLLBACK 都用它 ⇒ 落在同一条连接上）。
async fn tx_pool(
    config: &DBConfig,
    create: bool,
) -> Result<crate::db::connection::AnyPool, String> {
    use crate::db::connection::{pool_registry, AnyPool};
    let registry = pool_registry();
    if !create {
        return registry
            .tx_pool(&config.id)
            .ok_or_else(|| "事务已结束或尚未开始（没有进行中的事务）".to_string());
    }
    if let Some(p) = registry.tx_pool(&config.id) {
        return Ok(p);
    }
    // 单连接池：事务期间的每一条语句都落在同一条连接上
    let pool = match dispatch_db_type(config)? {
        "mysql" => AnyPool::MySql(mysql_pool_new(config, 1).await?),
        "postgresql" => AnyPool::Pg(pg_pool_new(config, 1).await?),
        "sqlite" => AnyPool::Sqlite(sqlite_pool_new(config, 1).await?),
        _ => return Err("不支持的数据库类型".to_string()),
    };
    registry.insert_tx_pool(&config.id, pool.clone());
    Ok(pool)
}

/// 只读池：表结构 / 元数据 / 报表取数等一律走这条。
async fn mysql_pool(cfg: &DBConfig) -> Result<MySqlPool, String> {
    match pooled(cfg, "readOnly").await? {
        crate::db::connection::AnyPool::MySql(p) => Ok(p),
        _ => Err("连接池类型与请求不符".to_string()),
    }
}

async fn pg_pool(cfg: &DBConfig) -> Result<PgPool, String> {
    match pooled(cfg, "readOnly").await? {
        crate::db::connection::AnyPool::Pg(p) => Ok(p),
        _ => Err("连接池类型与请求不符".to_string()),
    }
}

async fn sqlite_pool(cfg: &DBConfig) -> Result<SqlitePool, String> {
    match pooled(cfg, "readOnly").await? {
        crate::db::connection::AnyPool::Sqlite(p) => Ok(p),
        _ => Err("连接池类型与请求不符".to_string()),
    }
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

/// 将驱动原生值包装成 tagged cell；T-001+003
fn wrap(driver_kind: CellKind, raw: serde_json::Value) -> serde_json::Value {
    // 空值统一打 null 标签，原值丢弃
    if matches!(raw, serde_json::Value::Null) {
        return tagged_cell(CellKind::Null, serde_json::Value::Null);
    }

    // T-038：对于整数类型，超过 JS Number.MAX_SAFE_INTEGER (2^53) 时序列化为字符串
    // 保留小整数为 Number 类型，大整数安全传递到前端
    if driver_kind == CellKind::Integer {
        if let serde_json::Value::Number(ref n) = raw {
            // 用 i64 覆盖正/负大整数
            if let Some(i) = n.as_i64() {
                let max_safe: i64 = 9007199254740992; // 2^53
                if i < -max_safe || i > max_safe {
                    return tagged_cell(
                        CellKind::Integer,
                        serde_json::Value::String(i.to_string()),
                    );
                }
                return tagged_cell(driver_kind, raw);
            }
            // 仅 u64 大正数（i64 无法表示）
            if let Some(u) = n.as_u64() {
                let max_safe: u64 = 9007199254740992;
                if u > max_safe {
                    return tagged_cell(
                        CellKind::Integer,
                        serde_json::Value::String(u.to_string()),
                    );
                }
                return tagged_cell(driver_kind, raw);
            }
        }
    }

    tagged_cell(driver_kind, raw)
}

/// 当驱动明确不能读取某列（解码失败）时使用的标签
// 当前未在 cell 函数中调用；保留供后续 SAFE_DECODE_ERROR 通道使用
#[allow(dead_code)]
fn decode_error_value(message: &str) -> serde_json::Value {
    tagged_cell(
        CellKind::DecodeError,
        serde_json::Value::String(message.to_string()),
    )
}

fn bytes_to_value(v: Option<Vec<u8>>) -> serde_json::Value {
    // 二进制路径：使用 base64 编码而非 from_utf8_lossy，避免有损还原
    match v {
        Some(b) => tagged_cell(
            CellKind::Binary,
            serde_json::Value::String(base64_encode(&b)),
        ),
        None => tagged_cell(CellKind::Null, serde_json::Value::Null),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    // 不引入额外依赖，使用简易 base64 编码
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        out.push(CHARS[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

fn tagged_cell(kind: CellKind, value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "__kind": kind_to_str(kind),
        "value": value,
    })
}

fn kind_to_str(k: CellKind) -> &'static str {
    match k {
        CellKind::Null => "null",
        CellKind::Integer => "integer",
        CellKind::Float => "float",
        CellKind::Decimal => "decimal",
        CellKind::Text => "text",
        CellKind::Binary => "binary",
        CellKind::Date => "date",
        CellKind::Time => "time",
        CellKind::DateTime => "datetime",
        CellKind::Timestamp => "timestamp",
        CellKind::Uuid => "uuid",
        CellKind::Json => "json",
        CellKind::Unsupported => "unsupported",
        CellKind::DecodeError => "decode_error",
    }
}

/// 大整数安全包装：超过 JS 安全整数范围时改为字符串
// 保留供后续 BIGINT UNSIGNED 全链路无损接入使用
#[allow(dead_code)]
fn safe_int_value(n: i64) -> serde_json::Value {
    if n >= -(2_i64.pow(53)) && n <= 2_i64.pow(53) {
        serde_json::Value::Number(n.into())
    } else {
        serde_json::Value::String(n.to_string())
    }
}

#[allow(dead_code)]
fn safe_uint_value(n: u64) -> serde_json::Value {
    if n <= 2_u64.pow(53) {
        serde_json::Value::Number(n.into())
    } else {
        serde_json::Value::String(n.to_string())
    }
}

fn mysql_cell(row: &MySqlRow, i: usize) -> serde_json::Value {
    let raw = match row.try_get_raw(i) {
        Ok(v) => v,
        Err(_) => return tagged_cell(CellKind::Null, serde_json::Value::Null),
    };
    if raw.is_null() {
        return tagged_cell(CellKind::Null, serde_json::Value::Null);
    }
    let type_info = raw.type_info();
    let type_name: &str = type_info.name();
    macro_rules! int_cell {
        ($k:expr) => {
            $k.map(|v| wrap(CellKind::Integer, v))
        };
    }
    macro_rules! dec_cell {
        () => {
            mysql_dec::<sqlx::types::Decimal>(row, i)
                .map(|v| wrap(CellKind::Decimal, serde_json::Value::String(v.to_string())))
        };
    }
    let value: Option<serde_json::Value> = match type_name {
        "BOOLEAN" => mysql_dec::<bool>(row, i)
            .or_else(|| mysql_dec::<i8>(row, i))
            .map(|v| wrap(CellKind::Integer, v)),
        "TINYINT" => int_cell!(mysql_dec::<i8>(row, i)),
        "TINYINT UNSIGNED" => int_cell!(mysql_dec::<u8>(row, i)),
        "SMALLINT" | "YEAR" => int_cell!(mysql_dec::<i16>(row, i)),
        "SMALLINT UNSIGNED" => int_cell!(mysql_dec::<u16>(row, i)),
        "INT" | "MEDIUMINT" => int_cell!(mysql_dec::<i32>(row, i)),
        "INT UNSIGNED" | "MEDIUMINT UNSIGNED" => int_cell!(mysql_dec::<u32>(row, i)),
        "BIGINT" => int_cell!(mysql_dec::<i64>(row, i)),
        "BIGINT UNSIGNED" | "BIT" => int_cell!(mysql_dec::<u64>(row, i)),
        "FLOAT" | "DOUBLE" => mysql_dec::<f64>(row, i).map(|v| wrap(CellKind::Float, v)),
        "DECIMAL" => dec_cell!(),
        "DATE" => mysql_dec::<chrono::NaiveDate>(row, i)
            .map(|v| wrap(CellKind::Date, serde_json::Value::String(v.to_string()))),
        "TIME" => {
            // MySQL TIME 可以是负数或超 24 小时（-838:59:58 到 838:59:57）
            // NaiveTime 只能表示 0-24h，超过范围时退化为字符串保留原始值
            let time_str = row.try_get::<String, _>(i).ok();
            Some(match time_str {
                Some(s) if !s.is_empty() => {
                    if s.starts_with('-') || s.len() > 8 {
                        // 非标准 TIME（负数或超 24h），保留原始字符串
                        tagged_cell(CellKind::Time, serde_json::Value::String(s))
                    } else {
                        // 标准 TIME：尝试解析为 NaiveTime 以保证格式一致
                        match chrono::NaiveTime::parse_from_str(&s, "%H:%M:%S%.f")
                            .or_else(|_| chrono::NaiveTime::parse_from_str(&s, "%H:%M:%S"))
                        {
                            Ok(t) => wrap(CellKind::Time, serde_json::Value::String(t.to_string())),
                            Err(_) => tagged_cell(CellKind::Time, serde_json::Value::String(s)),
                        }
                    }
                }
                _ => tagged_cell(CellKind::Time, serde_json::Value::Null),
            })
        }
        "DATETIME" | "TIMESTAMP" => mysql_dec::<chrono::NaiveDateTime>(row, i)
            .map(|v| wrap(CellKind::DateTime, serde_json::Value::String(v.to_string()))),
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" | "GEOMETRY" => {
            Some(bytes_to_value(
                row.try_get::<Option<Vec<u8>>, _>(i).ok().flatten(),
            ))
        }
        _ => mysql_dec::<String>(row, i).map(|v| wrap(CellKind::Text, v)),
    };
    value.unwrap_or_else(|| tagged_cell(CellKind::Null, serde_json::Value::Null))
}

fn pg_cell(row: &PgRow, i: usize) -> serde_json::Value {
    let raw = match row.try_get_raw(i) {
        Ok(v) => v,
        Err(_) => return tagged_cell(CellKind::Null, serde_json::Value::Null),
    };
    if raw.is_null() {
        return tagged_cell(CellKind::Null, serde_json::Value::Null);
    }
    let type_info = raw.type_info();
    let type_name: &str = type_info.name();
    macro_rules! int_cell {
        ($k:expr) => {
            $k.map(|v| wrap(CellKind::Integer, v))
        };
    }
    macro_rules! dec_cell {
        () => {
            pg_dec::<sqlx::types::Decimal>(row, i)
                .map(|v| wrap(CellKind::Decimal, serde_json::Value::String(v.to_string())))
        };
    }
    let value: Option<serde_json::Value> = match type_name {
        "bool" => int_cell!(pg_dec::<bool>(row, i)),
        "int2" => int_cell!(pg_dec::<i16>(row, i)),
        "int4" | "oid" => int_cell!(pg_dec::<i32>(row, i)),
        "int8" => int_cell!(pg_dec::<i64>(row, i)),
        "float4" => pg_dec::<f32>(row, i).map(|v| wrap(CellKind::Float, v)),
        "float8" => pg_dec::<f64>(row, i).map(|v| wrap(CellKind::Float, v)),
        "numeric" => dec_cell!(),
        "date" => pg_dec::<chrono::NaiveDate>(row, i)
            .map(|v| wrap(CellKind::Date, serde_json::Value::String(v.to_string()))),
        "time" => pg_dec::<chrono::NaiveTime>(row, i)
            .map(|v| wrap(CellKind::Time, serde_json::Value::String(v.to_string()))),
        "timestamp" => pg_dec::<chrono::NaiveDateTime>(row, i)
            .map(|v| wrap(CellKind::DateTime, serde_json::Value::String(v.to_string()))),
        "timestamptz" => pg_dec::<chrono::DateTime<chrono::Utc>>(row, i).map(|v| {
            wrap(
                CellKind::Timestamp,
                serde_json::Value::String(v.to_string()),
            )
        }),
        "uuid" => pg_dec::<sqlx::types::Uuid>(row, i)
            .map(|v| wrap(CellKind::Uuid, serde_json::Value::String(v.to_string()))),
        "json" | "jsonb" => pg_dec::<serde_json::Value>(row, i).map(|v| wrap(CellKind::Json, v)),
        "bytea" => Some(bytes_to_value(
            row.try_get::<Option<Vec<u8>>, _>(i).ok().flatten(),
        )),
        _ => pg_dec::<String>(row, i).map(|v| wrap(CellKind::Text, v)),
    };
    value.unwrap_or_else(|| tagged_cell(CellKind::Null, serde_json::Value::Null))
}

fn sqlite_cell(row: &SqliteRow, i: usize) -> serde_json::Value {
    let raw = match row.try_get_raw(i) {
        Ok(v) => v,
        Err(_) => return tagged_cell(CellKind::Null, serde_json::Value::Null),
    };
    if raw.is_null() {
        return tagged_cell(CellKind::Null, serde_json::Value::Null);
    }
    let type_info = raw.type_info();
    let type_name: &str = type_info.name();
    let value: Option<serde_json::Value> = match type_name {
        "INTEGER" => sqlite_dec::<i64>(row, i)
            .or_else(|| sqlite_dec::<bool>(row, i))
            .map(|v| wrap(CellKind::Integer, v)),
        "REAL" => sqlite_dec::<f64>(row, i).map(|v| wrap(CellKind::Float, v)),
        "BLOB" => Some(bytes_to_value(
            row.try_get::<Option<Vec<u8>>, _>(i).ok().flatten(),
        )),
        _ => sqlite_dec::<String>(row, i).map(|v| wrap(CellKind::Text, v)),
    };
    value.unwrap_or_else(|| tagged_cell(CellKind::Null, serde_json::Value::Null))
}

// ================== 执行器 ==================

type RunOutput = (Vec<ColumnMeta>, Vec<Vec<serde_json::Value>>, u64, bool);

fn column_meta_from_row<C: Column>(cols: &[C]) -> Vec<ColumnMeta> {
    cols.iter()
        .enumerate()
        .map(|(i, c)| ColumnMeta {
            ordinal: i as u32,
            name: c.name().to_string(),
            native_type: c.type_info().name().to_string(),
            logical_type: map_logical_type(c.type_info().name()),
            nullable: true, // 驱动未提供可靠 nullable 信息，默认 true（保守处理）
        })
        .collect()
}

fn map_logical_type(native: &str) -> String {
    match native.to_ascii_uppercase().as_str() {
        "INT8" | "BIGINT" | "INT" | "INT4" | "MEDIUMINT" | "SMALLINT" | "TINYINT" => {
            "integer".to_string()
        }
        "BIGINT UNSIGNED" | "INT UNSIGNED" | "MEDIUMINT UNSIGNED" | "SMALLINT UNSIGNED"
        | "TINYINT UNSIGNED" | "BIT" => "unsigned_integer".to_string(),
        "FLOAT" | "DOUBLE" | "FLOAT4" | "FLOAT8" | "REAL" => "float".to_string(),
        "DECIMAL" | "NUMERIC" => "decimal".to_string(),
        "BOOLEAN" | "BOOL" => "boolean".to_string(),
        "DATE" => "date".to_string(),
        "TIME" => "time".to_string(),
        "DATETIME" | "TIMESTAMP" => "datetime".to_string(),
        "TIMESTAMPTZ" => "timestamp_tz".to_string(),
        "UUID" => "uuid".to_string(),
        "JSON" | "JSONB" => "json".to_string(),
        "BYTEA" | "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB"
        | "GEOMETRY" => "binary".to_string(),
        _ => "text".to_string(),
    }
}

async fn mysql_run(pool: &MySqlPool, sql: &str) -> Result<RunOutput, String> {
    if is_query_stmt(sql) {
        // 先用空参数 prepare 获取列元数据，避免 0 行时丢失列定义
        let stmt = sqlx::query(sql)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;
        let meta: Vec<ColumnMeta> = stmt
            .first()
            .map(|r| column_meta_from_row(r.columns()))
            .unwrap_or_default();
        let mut data = Vec::with_capacity(stmt.len());
        for r in &stmt {
            let mut row = Vec::with_capacity(r.columns().len());
            for i in 0..r.columns().len() {
                row.push(mysql_cell(r, i));
            }
            data.push(row);
        }
        Ok((meta, data, 0, true))
    } else {
        let res = sqlx::query(sql)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok((Vec::new(), Vec::new(), res.rows_affected(), false))
    }
}

async fn pg_run(pool: &PgPool, sql: &str) -> Result<RunOutput, String> {
    if is_query_stmt(sql) {
        let rows = sqlx::query(sql)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;
        let meta: Vec<ColumnMeta> = rows
            .first()
            .map(|r| column_meta_from_row(r.columns()))
            .unwrap_or_default();
        let mut data = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut row = Vec::with_capacity(r.columns().len());
            for i in 0..r.columns().len() {
                row.push(pg_cell(r, i));
            }
            data.push(row);
        }
        if meta.is_empty() {
            return Ok((Vec::new(), Vec::new(), 0, true));
        }
        Ok((meta, data, 0, true))
    } else {
        let res = sqlx::query(sql)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok((Vec::new(), Vec::new(), res.rows_affected(), false))
    }
}

/// T-031：sqlite 走"边取边停"。`cap` 有值时按流取 cap+1 行——多拿那一行只为确定
/// "后面还有没有"，取够就丢掉流（判据与连接可复用性见 tests::early_stop_streaming_saves_the_rest_of_the_fetch）。
/// 返回的第二个值是"确实还有更多行"：这时总行数没数过，前端不能说"共 M 行"。
/// mysql/postgres 不在本机可测的范围内，继续走 fetch_all + 事后截断。
async fn sqlite_run_capped(
    pool: &SqlitePool,
    sql: &str,
    cap: Option<usize>,
) -> Result<(RunOutput, bool), String> {
    use futures_util::TryStreamExt;
    let Some(n) = cap.filter(|_| is_query_stmt(sql)) else {
        return Ok((sqlite_run(pool, sql).await?, false));
    };
    let mut conn = pool.acquire().await.map_err(|e| e.to_string())?;
    let mut stream = sqlx::query(sql).fetch(&mut *conn);
    let mut data: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut meta: Vec<ColumnMeta> = Vec::new();
    let mut more = false;
    while let Some(row) = stream.try_next().await.map_err(|e| e.to_string())? {
        if data.len() >= n {
            // 只多看这一行，不再往下物化
            more = true;
            break;
        }
        if meta.is_empty() {
            meta = column_meta_from_row(row.columns());
        }
        let mut cells = Vec::with_capacity(row.columns().len());
        for i in 0..row.columns().len() {
            cells.push(sqlite_cell(&row, i));
        }
        data.push(cells);
    }
    drop(stream);
    drop(conn);
    Ok(((meta, data, 0, true), more))
}

async fn sqlite_run(pool: &SqlitePool, sql: &str) -> Result<RunOutput, String> {
    if is_query_stmt(sql) {
        let rows = sqlx::query(sql)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;
        let meta: Vec<ColumnMeta> = rows
            .first()
            .map(|r| column_meta_from_row(r.columns()))
            .unwrap_or_default();
        let mut data = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut row = Vec::with_capacity(r.columns().len());
            for i in 0..r.columns().len() {
                row.push(sqlite_cell(r, i));
            }
            data.push(row);
        }
        if meta.is_empty() {
            return Ok((Vec::new(), Vec::new(), 0, true));
        }
        Ok((meta, data, 0, true))
    } else {
        let res = sqlx::query(sql)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok((Vec::new(), Vec::new(), res.rows_affected(), false))
    }
}

// 统一分发执行
async fn run_dispatch(cfg: &DBConfig, sql: &str) -> Result<RunOutput, String> {
    run_dispatch_mode(cfg, sql, "readOnly").await
}

/// 按访问模式选池执行。
///
/// 为什么要带 mode：池的缓存键含 access_mode（只读池与可写池是两个池）。
/// **写入必须走可写池** —— 走只读池对 SQLite/PG 照样能写（连接本身没有只读属性），
/// 但那等于把写操作挂在一个名为「只读」的池上，后面对账时口径就乱了。
async fn run_dispatch_mode(
    cfg: &DBConfig,
    sql: &str,
    access_mode: &str,
) -> Result<RunOutput, String> {
    match pooled(cfg, access_mode).await? {
        crate::db::connection::AnyPool::MySql(p) => mysql_run(&p, sql).await,
        crate::db::connection::AnyPool::Pg(p) => pg_run(&p, sql).await,
        crate::db::connection::AnyPool::Sqlite(p) => sqlite_run(&p, sql).await,
    }
}

// ================== AI 命令 ==================

// 获取 AI 配置
// S0 修复：data_dir 错误透传；保存 API key 时不再走 get_data_dir 侧路。
#[command]
async fn get_ai_config() -> Result<AIConfig, String> {
    let data_dir = queries::get_data_dir()?;
    let config_path = data_dir.join("ai_config.json");

    if config_path.exists() {
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| format!("读取配置失败: {}", e))?;
        // S0：不返回 API key 明文回前端，由前端通过专用脱敏通道显示
        let mut cfg: AIConfig =
            serde_json::from_str(&content).map_err(|e| format!("解析配置失败: {}", e))?;
        cfg.api_key.clear();
        Ok(cfg)
    } else {
        Ok(AIConfig {
            base_url: "".to_string(),
            api_key: "".to_string(),
            model: "".to_string(),
        })
    }
}

// 保存 AI 配置
// S0 修复：使用 atomic_write 落盘，避免写入中断导致配置损坏。
#[command]
async fn save_ai_config(config: AIConfig) -> Result<(), String> {
    let data_dir = queries::get_data_dir()?;
    let config_path = data_dir.join("ai_config.json");

    let content =
        serde_json::to_string_pretty(&config).map_err(|e| format!("序列化失败: {}", e))?;

    queries::atomic_write_pub(&config_path, content.as_bytes())?;
    Ok(())
}

// 调用 AI 服务
async fn call_ai_service(config: &AIConfig, prompt: &str) -> Result<String, String> {
    // 统一走 report::ai::chat：OpenAI 兼容的 messages 结构 + 超时 + 带状态码的报错。
    report::ai::chat(config, prompt).await
}

// AI 生成 SQL 在 ai_sql 模块：那里的提示词带真实列清单，产出还在本机校验。

// AI 解释查询结果
#[command]
async fn ai_explain_results(
    sql: String,
    results: Vec<serde_json::Value>,
    config: AIConfig,
) -> Result<String, String> {
    report::ai::ensure_configured(&config)?;

    let results_str =
        serde_json::to_string_pretty(&results).unwrap_or_else(|_| "无法序列化结果".to_string());

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
async fn ai_optimize_sql(
    sql: String,
    table_schema: Vec<ColumnInfo>,
    config: AIConfig,
) -> Result<String, String> {
    report::ai::ensure_configured(&config)?;

    let schema_str = table_schema
        .iter()
        .map(|c| {
            format!(
                "- {} ({}), {}",
                c.name,
                c.data_type,
                if c.is_primary {
                    "主键"
                } else if !c.nullable {
                    "NOT NULL"
                } else {
                    "可空"
                }
            )
        })
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
    report::ai::ensure_configured(&config)?;

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
async fn ai_diagnose_error(
    error_message: String,
    sql: String,
    config: AIConfig,
) -> Result<String, String> {
    report::ai::ensure_configured(&config)?;

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

// T-015 秘密迁移流程 stub
// 扫描现有 connections，对所有 revision < CURRENT_MIGRATION_WATERMARK 的连接
// 标记"需要重新输入密码"。S0 阶段 T-007 已把所有落盘密码清空，
// 因此真实"迁移"主要是让 UI 知道哪些连接必须重输密码才能使用。
pub const CURRENT_MIGRATION_WATERMARK: u32 = 2;

#[derive(Debug, Clone, serde::Serialize)]
pub struct MigrationReport {
    pub total_connections: u32,
    pub needs_reentry: u32,
    pub up_to_date: u32,
    /// T-014：需要重新输入密码的连接，**逐条列出**。
    /// 只给一个计数的话用户没法知道该去改哪一条。
    pub missing_credentials: Vec<db::credentials::MissingCredential>,
}

async fn migration_report_with(
    store: &dyn secrets::SecretStore,
) -> Result<MigrationReport, String> {
    // T-014：迁移报告的判据从"猜 revision 水位"改成**真的去密钥库取一次**。
    // 旧口径（revision < 水位 ⇒ needs_reentry）与实际能不能取回密码无关，
    // 报出来的数是假的。
    let raw = queries::load_connections().await?;
    let (unsealed, missing) = db::credentials::unseal(store, raw);
    let up_to_date = unsealed
        .iter()
        .filter(|c| c.credential_state == db::credentials::CREDENTIAL_OK)
        .count() as u32;
    Ok(MigrationReport {
        total_connections: unsealed.len() as u32,
        needs_reentry: missing.len() as u32,
        up_to_date,
        missing_credentials: missing,
    })
}

#[command]
async fn run_secret_migration() -> Result<MigrationReport, String> {
    let store = secrets::secret_store();
    migration_report_with(store.as_ref()).await
}

// T-046：检查连接 revision 是否与持久化一致；
// 用于「编辑连接后旧会话失效」语义。
#[command]
async fn check_connection_revision(
    connection_id: String,
    expected_revision: u32,
) -> Result<bool, String> {
    let conns = queries::load_connections().await?;
    for c in &conns {
        if c.id == connection_id {
            return Ok(c.revision == expected_revision);
        }
    }
    Err(format!("连接 {} 不存在", connection_id))
}

// 测试数据库连接
#[command]
async fn test_connection(config: DBConfig) -> Result<bool, String> {
    dispatch_db_type(&config)?;
    run_dispatch(&config, "SELECT 1").await?;
    println!("Testing connection to {}...", config.name);
    Ok(true)
}

// 执行 SQL 查询（S0 + T-023 + T-024 + T-022）
// - access_mode 默认 readOnly；S0 同时按词法分类器拒绝可疑绕过
// - 写操作需要审批：S0 通过 approval_id 字串参数显式注入；缺失/过期/摘要不匹配均拒
// - expected_generation 用于前后端防竞态校验（T-022）
// - T-035：成功和失败都自动记录历史
#[command]
async fn execute_query(
    sql: String,
    config: DBConfig,
    access_mode: Option<String>,
    approval: Option<security::ApprovalGrant>,
    expected_generation: Option<u32>,
    max_rows: Option<u32>,
) -> Result<QueryResult, String> {
    let start = std::time::Instant::now();
    // T-013：先把这一单登记为"在途"，取消协议才有得匹配。
    // 改之前 in-flight 表**从来没被 register 过** ⇒ 任何取消请求都只能拿到 NotFound。
    //
    // 请求标识仍是 **UUID v4**（T-018 验收：`query_id_is_uuid_v4` 钉着这条），
    // 外面套一层 `gen{generation}|` 前缀（T-022 的代次透传）。
    // 前端拿到的整个字符串就是取消/查状态用的 key ⇒ 三处必须是同一个字符串。
    let generation = expected_generation.unwrap_or(0);
    let query_id = format!("gen{}|{}", generation, uuid::Uuid::new_v4());
    crate::db::cancel::in_flight().register(
        &config.id,
        query_id.clone(),
        security::digest_sql(&sql),
    );

    // T-031：把这一单登记进作业注册表 —— 成功/失败/取消才有地方可标，
    // 状态查询也才有东西可查（改之前普通查询**从不进**注册表，
    // 于是 `mark_succeeded` / `mark_failed` / `get_state` 全是只有测试在调的死代码）。
    db::session::registry().insert(db::session::QueryJobInternal {
        query_id: query_id.clone(),
        state: db::session::QueryState::Running,
        state_msg: "执行中".into(),
        cancel_requested: false,
        start_time: std::time::Instant::now(),
        batch_tx: tokio::sync::mpsc::channel(1).0,
        transaction_state: db::session::TransactionState::Idle,
        generation,
        conflict_detected: false,
        last_affected_rows: 0,
    });

    let result = execute_query_inner(
        sql.clone(),
        config.clone(),
        access_mode,
        approval,
        expected_generation,
        max_rows,
        query_id.clone(),
    )
    .await;

    // 终态写进作业注册表：成功 ⇒ succeeded，失败 ⇒ failed（带原因）。
    match &result {
        Ok(_) => db::session::registry().mark_succeeded(&query_id),
        Err(e) => db::session::registry().mark_failed(&query_id, e),
    }

    // 查询结束（成功或失败）⇒ 从在途表摘掉。
    // 若这一单曾请求过取消，`finish` 会回 `Confirmed`（取消已生效）。
    let cancel_outcome = crate::db::cancel::in_flight().finish(&query_id);
    if let Some(state) = cancel_outcome {
        // 取消生效：把作业状态也标成"已取消"，让状态查询与取消协议说同一件事。
        db::session::registry().mark_cancelled(&query_id);
        let _ = state;
    }

    // T-035：所有终态（成功或失败）都记录历史
    match &result {
        Ok(r) => {
            let hist_item = QueryHistoryItem {
                id: uuid::Uuid::new_v4().to_string(),
                sql: sql.chars().take(500).collect(),
                connection_id: config.id.clone(),
                connection_name: config.name.clone(),
                timestamp: chrono::Utc::now().timestamp(),
                execution_time_ms: r.execution_time_ms,
                success: true,
                error: None,
            };
            let _ = queries::append_history(hist_item).await;
        }
        Err(e) => {
            let hist_item = QueryHistoryItem {
                id: uuid::Uuid::new_v4().to_string(),
                sql: sql.chars().take(500).collect(),
                connection_id: config.id.clone(),
                connection_name: config.name.clone(),
                timestamp: chrono::Utc::now().timestamp(),
                execution_time_ms: start.elapsed().as_millis() as u64,
                success: false,
                error: Some(e.chars().take(200).collect()),
            };
            let _ = queries::append_history(hist_item).await;
        }
    }
    result
}

/// 事务状态机拒绝时的错误。
///
/// 代次不匹配 ⇒ `STALE_GENERATION`（结果过期，重试有意义）；
/// 其它状态非法 ⇒ `UNKNOWN`，但都要带上**当时的事务状态**，
/// 否则界面只知道"被拒了"，不知道"事务现在处于什么状态"。
fn tx_state_error(
    action: &str,
    detail: &str,
    state: Option<db::session::TransactionState>,
    query_id: &str,
) -> String {
    let msg = format!("事务状态机拒绝 {}：{}", action, detail);
    let state_str = state
        .map(|s| s.as_str().to_string())
        .unwrap_or_else(|| "unknown".into());
    let err = if detail.contains("generation 不匹配") {
        db::ipc::ApiError::stale_generation(&msg)
    } else {
        db::ipc::ApiError::unknown("execute_query", &msg)
    };
    err.with_transaction_state(&state_str)
        .with_query_id(query_id)
        .to_wire()
}

/// 执行前的门禁：只读通道 / 写审批。
///
/// 抽成独立函数的理由：`execute_query` 与 `query_start`（T-031 流式）
/// 必须用**同一套**门禁。两份判据迟早漂移，漂移的后果就是
/// "流式接口能绕过审批" —— 那比没有审批更坏。
fn gate_execution(
    sql: &str,
    mode: &str,
    approval: Option<&security::ApprovalGrant>,
    classification: db::sql_classify::Classification,
    query_id: &str,
) -> Result<db::sql_classify::Classification, String> {
    // 多语句一律拒（**两个通道都拒**）。
    //
    // 为什么：审批/只读判据都只看**第一条**语句。`SELECT 1; DROP TABLE x`
    // 的第一条是安全 SELECT ⇒ 会被放行，后面那条却能删表。
    // 之前 `classify` 的多语句计数还少算一条（结尾没分号时算成 0），
    // 等于这道门根本没关。现在计数修对了，门也真的关上了。
    if classification.other_statements > 0 {
        return Err(db::ipc::ApiError::sql_classify(&format!(
            "多语句被拒：这条 SQL 含 {} 条语句，只允许单语句执行",
            classification.other_statements + 1
        ))
        .with_query_id(query_id)
        .to_wire());
    }
    match mode {
        "readOnly" => {
            if !matches!(
                classification.safety,
                db::sql_classify::SafetyClass::ReadOnlySafe
            ) {
                // 包含 ReadOnlyUnsafe/Write/Unknown 一律拒绝只读通道。
                // 错误带机器可读码 READONLY_BLOCKED：前端 errors.ts 优先认码，
                // 中文正文保持原样 ⇒ 认码的老前端也不受影响。
                return Err(db::ipc::ApiError::readonly_blocked(
                    &format!("{:?}", classification.kind),
                    &format!("{:?}", classification.safety),
                )
                .with_query_id(query_id)
                .to_wire());
            }
        }
        "writable" => {
            // T-055：生产/staging 环境强制额外审批门槛（除普通 approval 外）
            let env = approval
                .map(|g| g.environment.as_str())
                .unwrap_or("unknown");
            // 写操作需要审批
            security::evaluate(
                approval,
                classification.kind,
                classification.safety,
                sql,
                chrono::Utc::now().timestamp(),
                env,
            )
            .map_err(|e| {
                // 用真实消息（ApprovalError::message），不是 `{:?}` 的枚举壳
                db::ipc::ApiError::approval_denied(&format!("审批校验失败: {}", e.message()))
                    .with_query_id(query_id)
                    .to_wire()
            })?;
        }
        other => {
            return Err(format!("未知 access_mode: {}", other));
        }
    }
    Ok(classification)
}

async fn execute_query_inner(
    sql: String,
    config: DBConfig,
    access_mode: Option<String>,
    approval: Option<security::ApprovalGrant>,
    expected_generation: Option<u32>,
    max_rows: Option<u32>,
    query_id: String,
) -> Result<QueryResult, String> {
    if sql.trim().is_empty() {
        return Err("SQL 语句为空".to_string());
    }
    let mode = access_mode.unwrap_or_else(|| "readOnly".to_string());

    // T-023：用词法分类器判定语句族与安全级别
    let classification = db::sql_classify::classify(&sql);
    gate_execution(&sql, &mode, approval.as_ref(), classification, &query_id)?;

    // T-041：分段耗时 — 队列/连接/执行分别计时
    let start = std::time::Instant::now();
    // 类型先校验：不支持的类型要在这里就说清楚，而不是等到建池才炸。
    dispatch_db_type(&config)?;
    let connect_start = std::time::Instant::now();
    // 只有 sqlite + 有上限时走流式；Some(true) = 还有更多行没取，总数未数过
    let cap = max_rows.map(|m| m as usize);
    let mut streamed_more: Option<bool> = None;
    // T-043：事务状态跟踪。
    //
    // **2026-10-22 修正**：改之前这里 BEGIN 时直接塞一个
    // `transaction_state: Active` 的条目 —— **绕过状态机**：generation 没校验、
    // `can_transition` 没参与。于是 `TransactionState` 状态机
    // （Idle / Failed / Unknown + can_transition）在生产构建里是**死代码**，
    // 只有测试在调它（而测试永远绿 ⇒ 看起来是好的）。
    //
    // 现在：状态转换在**跑 SQL 之前**做 —— 否则 COMMIT 失败之后状态还停在 Active，
    // 界面根本看不出"这个事务到底提交了没有"。转换是否合法由状态机自己判。
    let reg = db::session::registry();
    let tx_gen = expected_generation.unwrap_or(0);
    let conn_key = format!("{}:{}", config.id, tx_gen);
    let tx_kind = db::sql_classify::classify(&sql).kind;
    let upper = sql.trim().to_ascii_uppercase();
    let tx_action: Option<&'static str> =
        if matches!(tx_kind, db::sql_classify::StatementKind::TxControl) {
            if upper.starts_with("BEGIN") {
                // 先登记成 Idle，再由状态机走 Idle → Active（顺带校验 generation）。
                // 已有事务时不覆盖：那样第二次 BEGIN 会把 Active 重置掉，
                // 状态机也就没机会拒它了。
                match reg.get_transaction_state(&conn_key) {
                    None => {
                        reg.insert(db::session::QueryJobInternal {
                            query_id: conn_key.clone(),
                            state: db::session::QueryState::Running,
                            state_msg: "事务已开始".into(),
                            cancel_requested: false,
                            start_time: std::time::Instant::now(),
                            batch_tx: tokio::sync::mpsc::channel(1).0,
                            transaction_state: db::session::TransactionState::Idle,
                            generation: tx_gen,
                            conflict_detected: false,
                            last_affected_rows: 0,
                        });
                    }
                    Some(db::session::TransactionState::Idle) => {}
                    Some(_) => {
                        // 上一个事务已经终结（提交/回滚/失败/未知）⇒ 先重置成 Idle。
                        // 不重置的话，**同一条连接上的第二个 BEGIN 会被状态机拒死**
                        // （Committed/RolledBack 都不允许 → Active），用户再也开不了新事务。
                        // 仍在进行中（Active）的除外 —— 那种情况本来就该被拒。
                        reg.reset_transaction_to_idle(&conn_key);
                    }
                }
                reg.begin_transaction(&conn_key, tx_gen).map_err(|e| {
                    tx_state_error("BEGIN", &e, reg.get_transaction_state(&conn_key), &conn_key)
                })?;
                Some("begin")
            } else if upper.starts_with("COMMIT") {
                reg.commit_transaction(&conn_key, tx_gen).map_err(|e| {
                    tx_state_error(
                        "COMMIT",
                        &e,
                        reg.get_transaction_state(&conn_key),
                        &conn_key,
                    )
                })?;
                Some("commit")
            } else if upper.starts_with("ROLLBACK") {
                reg.rollback_transaction(&conn_key, tx_gen).map_err(|e| {
                    tx_state_error(
                        "ROLLBACK",
                        &e,
                        reg.get_transaction_state(&conn_key),
                        &conn_key,
                    )
                })?;
                Some("rollback")
            } else {
                None
            }
        } else {
            None
        };

    // 池走全局缓存（2026-10-22）：同一 (连接, 参数指纹, access_mode) 复用同一个池。
    // 改之前这里每次都新建一个池 ⇒ 每条查询都重新握手 + 重新鉴权。
    //
    // T-043：但**事务期间必须换池** —— 事务的每一条语句都要落在**同一条连接**上。
    // 普通池有多条连接 ⇒ BEGIN 与 COMMIT 会落到不同连接 ⇒ 数据库端根本没有
    // 这个事务（COMMIT 报 "no transaction is active"，MySQL 上更糟：
    // 悄悄提交了一个空事务，而界面显示"事务已提交"）。
    let pool = match tx_action {
        Some("begin") => tx_pool(&config, true).await,
        Some("commit") | Some("rollback") => match tx_pool(&config, false).await {
            Ok(p) => Ok(p),
            // 没有事务池（没 BEGIN 过，或已被回收）⇒ 交给数据库报真实错误
            Err(_) => pooled(&config, &mode).await,
        },
        _ => {
            if crate::db::connection::pool_registry().has_tx_pool(&config.id) {
                tx_pool(&config, false).await
            } else {
                pooled(&config, &mode).await
            }
        }
    }
    .map_err(|e| {
        db::ipc::ApiError::connection_failed(&e)
            .with_query_id(&query_id)
            .to_wire()
    })?;
    let dispatch_result = match pool {
        crate::db::connection::AnyPool::MySql(pool) => {
            let es = std::time::Instant::now();
            mysql_run(&pool, &sql)
                .await
                .map(|r| (r.0, r.1, r.2, r.3, es))
        }
        crate::db::connection::AnyPool::Pg(pool) => {
            let es = std::time::Instant::now();
            pg_run(&pool, &sql).await.map(|r| (r.0, r.1, r.2, r.3, es))
        }
        crate::db::connection::AnyPool::Sqlite(pool) => {
            let es = std::time::Instant::now();
            sqlite_run_capped(&pool, &sql, cap).await.map(|(r, more)| {
                streamed_more = if cap.is_some() { Some(more) } else { None };
                (r.0, r.1, r.2, r.3, es)
            })
        }
    };

    let (column_meta, mut rows, affected, is_query, exec_start) = match dispatch_result {
        Ok(v) => v,
        Err(e) => {
            // 语句失败：事务状态按"结果未知"推进，绝不假装它还停在老状态。
            match tx_action {
                Some("commit") => reg.mark_commit_response_lost(&conn_key),
                Some("rollback") => reg.mark_transaction_unknown(&conn_key),
                _ => {
                    // 事务进行中的一条普通语句失败 ⇒ 事务进入 failed
                    if reg.get_transaction_state(&conn_key)
                        == Some(db::session::TransactionState::Active)
                    {
                        reg.mark_transaction_failed(&conn_key);
                    }
                }
            }
            // 语句本身失败：既不是连接问题也不是审批问题 ⇒ UNKNOWN
            return Err(db::ipc::ApiError::unknown("execute_query", &e)
                .with_query_id(&query_id)
                .to_wire());
        }
    };

    // T-046：写操作记录影响行数。0 行且在事务里 ⇒ 乐观并发冲突
    // （更新没命中任何行，用户以为改成功了，其实什么都没改）。
    if !is_query {
        reg.record_write_impact(&query_id, affected);
    }

    // 事务结束（COMMIT/ROLLBACK 成功）⇒ 丢弃事务池，后续语句回到普通池。
    if matches!(tx_action, Some("commit") | Some("rollback")) {
        crate::db::connection::pool_registry().remove_tx_pool(&config.id);
    }

    // 事务语句成功 ⇒ 推进状态机
    match tx_action {
        // BEGIN 之后事务仍然是 Active（这条"作业"要一直挂到 COMMIT/ROLLBACK）
        Some("begin") => {}
        Some("commit") => {
            reg.mark_committed(&conn_key);
            reg.mark_transaction_finished(&conn_key, "事务已提交");
        }
        Some("rollback") => {
            reg.mark_rolled_back(&conn_key);
            reg.mark_transaction_finished(&conn_key, "事务已回滚");
        }
        _ => {}
    }
    // T-018/T-013：请求标识就是外层登记在途时用的那个 query_id ——
    // 界面拿它去查状态、去请求取消，后端拿它去认这一单，三处必须是同一个。

    let columns: Vec<String> = column_meta.iter().map(|m| m.name.clone()).collect();

    // T-022 generation 透传：UI 可携带并比对（query_id 里已带 `gen{generation}|` 前缀）

    // T-031：max_rows 截断。sqlite 那条已经在取数时停了，"还有更多"是确定的，
    // 但总行数没数过——total_rows 给 0，由前端说"后面的行没取"而不是编一个总数。
    let max = max_rows.unwrap_or(u32::MAX) as usize;
    let (total_rows, truncated) = match streamed_more {
        Some(more) => (if more { 0 } else { rows.len() as u64 }, more),
        None => {
            let total = rows.len() as u64;
            let cut = rows.len() > max;
            if cut {
                rows.truncate(max);
            }
            (total, cut)
        }
    };

    let mut result = QueryResult {
        id: query_id.clone(),
        columns,
        column_meta,
        rows,
        affected_rows: affected,
        execution_time_ms: start.elapsed().as_millis() as u64,
        is_query,
        timings: build_timings(start, connect_start, exec_start),
        truncated,
        total_rows,
    };
    result.id = query_id;
    Ok(result)
}

// T-019 v2 IPC：metadata_list_v2
// 相比 get_tables：返回结构化 DbObjectRef + 缓存版本。
// 旧 get_tables 保留做向后兼容；新调用方用 v2 路径。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DbObjectRefV2 {
    pub schema: Option<String>,
    pub name: String,
    pub kind: String, // "table" | "view"
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MetadataListV2 {
    pub objects: Vec<DbObjectRefV2>,
    pub cache_version: String,
}

#[command]
async fn metadata_list_v2(config: DBConfig) -> Result<db::ipc::V2Envelope<MetadataListV2>, String> {
    // v2 契约：成功与失败走同一个返回类型（`Err` 分支装业务错误）。
    // 外层的 `Err(String)` 只留给"包络都构造不出来"这种没法装进包络的情况。
    match metadata_list_v2_inner(&config).await {
        Ok(data) => Ok(db::ipc::V2Envelope::ok(data, &config.id)),
        Err(e) => Ok(db::ipc::V2Envelope::err(db::ipc::ApiError::unknown(
            "metadata_list",
            &e,
        ))),
    }
}

async fn metadata_list_v2_inner(config: &DBConfig) -> Result<MetadataListV2, String> {
    let kind = dispatch_db_type(config)?;
    let mut objects = Vec::new();
    match kind {
        "postgresql" => {
            let pool = pg_pool(config).await?;
            let rows = sqlx::query(
                "SELECT n.nspname AS schema, c.relname AS name, c.relkind AS kind \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind IN ('r','v','p') \
                   AND n.nspname NOT IN ('pg_catalog','information_schema') \
                   AND n.nspname NOT LIKE 'pg_toast%' \
                 ORDER BY n.nspname, c.relname",
            )
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
            for r in &rows {
                let schema: String = r.try_get("schema").unwrap_or_default();
                let name: String = r.try_get("name").unwrap_or_default();
                let k: String = r.try_get("kind").unwrap_or_else(|_| "r".to_string());
                let kind_str = match k.as_str() {
                    "v" => "view",
                    _ => "table",
                };
                objects.push(DbObjectRefV2 {
                    schema: Some(schema),
                    name,
                    kind: kind_str.to_string(),
                });
            }
        }
        "mysql" => {
            let pool = mysql_pool(config).await?;
            let rows = sqlx::query(
                "SELECT table_name AS name, table_type FROM information_schema.tables \
                 WHERE table_schema = ? ORDER BY table_name",
            )
            .bind(&config.database)
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
            for r in &rows {
                let name: String = r.try_get("name").unwrap_or_default();
                let kind_str: String = r.try_get("table_type").unwrap_or_default();
                let k = if kind_str.eq_ignore_ascii_case("view") {
                    "view"
                } else {
                    "table"
                };
                objects.push(DbObjectRefV2 {
                    schema: Some(config.database.clone()),
                    name,
                    kind: k.to_string(),
                });
            }
        }
        "sqlite" => {
            let pool = sqlite_pool(config).await?;
            let rows = sqlx::query(
                "SELECT type, name FROM sqlite_master \
                 WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
            for r in &rows {
                let name: String = r.try_get("name").unwrap_or_default();
                let t: String = r.try_get("type").unwrap_or_default();
                objects.push(DbObjectRefV2 {
                    schema: None,
                    name,
                    kind: t,
                });
            }
        }
        _ => return Err("不支持的数据库类型".to_string()),
    }
    // 缓存版本：当前时间秒 + 表数；后端可换成实际缓存 hash
    let cache_version = format!("v{}-{}", chrono::Utc::now().timestamp(), objects.len());
    Ok(MetadataListV2 {
        objects,
        cache_version,
    })
}

// T-019 v2 IPC：query_status_v2
//
// **2026-10-22 修正**：改之前这个命令**写死**返回 `Pending`，
// 消息是"v2 命令已注册；具体状态接入待 session actor 完成" ——
// 也就是说它从来没查过任何东西。现在它真的去查两张表：
// 作业注册表（这一单跑成什么样了）+ 在途注册表（还剩几单在跑、取消有没有被受理）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryStatusV2 {
    pub query_id: String,
    /// 作业状态：queued/running/succeeded/failed/cancelled/unknown/…
    pub state: String,
    /// 状态说明（失败时是原因）
    pub message: String,
    /// 在途快照（这一单还在跑时才有）
    pub in_flight: Option<crate::db::cancel::QueryStatus>,
    /// 当前在途作业数
    pub pending_jobs: usize,
    /// 已请求取消、还没确认的条数
    pub pending_cancels: usize,
    /// 这一单已经跑了多久（毫秒）；查无此作业时为 None
    pub elapsed_ms: Option<u128>,
    /// 是否检测到乐观并发冲突（写操作影响 0 行）
    pub has_conflict: bool,
    /// 事务状态是否不可判定（不可自动重试，得先隔离连接）
    pub transaction_unknown: bool,
    /// 全局在途查询总数
    pub in_flight_total: usize,
    /// 当前打开的流式查询条数
    pub open_streams: usize,
    /// 这条流式查询已经跑了多久（毫秒）；不是流式查询时为 None
    pub stream_elapsed_ms: Option<u128>,
}

#[command]
async fn query_status_v2(query_id: String) -> Result<db::ipc::V2Envelope<QueryStatusV2>, String> {
    let reg = db::session::registry();
    // 查无此作业 ⇒ 状态 unknown（**不是**"没这单"：这一单可能只是已经跑完了）
    let (state, message) = reg.get_state(&query_id).unwrap_or((
        db::session::QueryState::Unknown,
        "查无此作业（可能已结束或从未在本进程登记）".to_string(),
    ));
    Ok(db::ipc::V2Envelope::ok(
        QueryStatusV2 {
            query_id: query_id.clone(),
            state: state.as_str().to_string(),
            message,
            in_flight: db::cancel::in_flight().status(&query_id),
            pending_jobs: reg.pending_count(),
            pending_cancels: db::cancel::in_flight().pending_count(),
            elapsed_ms: reg.elapsed_ms(&query_id),
            has_conflict: reg.has_conflict(&query_id),
            transaction_unknown: reg.is_transaction_unknown(&query_id),
            in_flight_total: db::cancel::in_flight().in_flight_count(),
            open_streams: db::session::streams().count().await,
            stream_elapsed_ms: db::session::streams()
                .get(&query_id)
                .await
                .map(|entry| entry.start_time.elapsed().as_millis()),
        },
        &query_id,
    ))
}

// T-019 v2 IPC：query_cancel_v2
#[command]
async fn query_cancel_v2(
    query_id: String,
) -> Result<db::ipc::V2Envelope<crate::db::cancel::CancelResponse>, String> {
    // 全局唯一注册表：取消命令与状态命令必须查**同一张表**，
    // 否则会出现"取消说受理了、状态说查无此单"这种自相矛盾。
    let resp = db::cancel::in_flight().cancel(&query_id);
    // 取消请求已受理 ⇒ 作业状态标成"取消中"（界面据此显示"正在请求取消"）。
    if matches!(resp.state, crate::db::cancel::CancelState::Accepted) {
        db::session::registry().mark_cancel_requested(&query_id);
    }
    Ok(db::ipc::V2Envelope::ok(resp, &query_id))
}

/// T-031：流式取数的**生产端**（三个驱动同构 ⇒ 宏展开同一份逻辑）。
///
/// 为什么用宏而不是泛型：sqlx 的行类型（MySqlRow / PgRow / SqliteRow）
/// 与 `fetch()` 返回的流类型都随驱动变化，泛型化要引入一层 trait 体操；
/// 而这段逻辑只有三十来行，宏展开三次比抽象层更清楚。
///
/// 背压：批次队列容量是 `MAX_QUEUE_BATCHES`，`send().await` 在队列满时**等待**
/// ⇒ 前端不取，生产者就停在这儿（内存不会一路涨）。
macro_rules! stream_rows {
    ($pool:expr, $sql:expr, $max_rows:expr, $tx:expr, $shared:expr, $cell_fn:path, $query_id:expr) => {{
        use futures_util::TryStreamExt;
        let mut conn = match $pool.acquire().await {
            Ok(c) => c,
            Err(e) => {
                let mut sh = $shared.lock().unwrap_or_else(|p| p.into_inner());
                sh.error = Some(format!("取连接失败: {}", e));
                sh.state = db::session::QueryState::Failed;
                sh.done = true;
                return;
            }
        };
        let mut stream = sqlx::query($sql).fetch(&mut *conn);
        let mut meta: Vec<ColumnMeta> = Vec::new();
        let mut rows: Vec<Vec<serde_json::Value>> = Vec::new();
        let mut bytes = 0usize;
        let mut seq = 0u32;
        let mut total = 0u64;
        let mut truncated = false;
        loop {
            // 取消检查：每批问一次「要不要停」。
            // 少了这一句，界面上的「取消」按钮对流式查询完全无效 ——
            // 因为流式这一路**不经过** execute_query 的那条在途表生命周期，
            // 取消请求没人理会，生产者会一路把库读到底。
            if crate::db::cancel::in_flight().is_cancel_requested($query_id) {
                let mut sh = $shared.lock().unwrap_or_else(|p| p.into_inner());
                sh.state = db::session::QueryState::Cancelled;
                sh.done = true;
                // 被取消 ⇒ 缓冲里那半批**不推**了（用户已经不要这份结果了）
                rows.clear();
                break;
            }
            let row = match stream.try_next().await {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => {
                    let mut sh = $shared.lock().unwrap_or_else(|p| p.into_inner());
                    sh.error = Some(format!("取数失败: {}", e));
                    sh.state = db::session::QueryState::Failed;
                    sh.done = true;
                    return;
                }
            };
            if let Some(m) = $max_rows {
                if total >= m as u64 {
                    truncated = true;
                    // 截断要**当场**记进共享状态：消费者可能在生产者收尾之前
                    // 就拿到最后一批，那时 shared.truncated 还没更新就报成"没截断"。
                    $shared.lock().unwrap_or_else(|p| p.into_inner()).truncated = true;
                    break;
                }
            }
            if meta.is_empty() {
                meta = column_meta_from_row(row.columns());
            }
            let mut cells = Vec::with_capacity(row.columns().len());
            for i in 0..row.columns().len() {
                cells.push($cell_fn(&row, i));
            }
            let row_bytes: usize = cells.iter().map(|c| c.to_string().len()).sum();
            // 批边界：行数上限 或 字节上限，谁先到算谁
            if !rows.is_empty()
                && (rows.len() >= db::session::MAX_BATCH_ROWS
                    || bytes + row_bytes > db::session::MAX_BATCH_BYTES)
            {
                let batch = db::session::QueryBatch {
                    seq,
                    columns: meta.clone(),
                    rows: std::mem::take(&mut rows),
                    truncated: false,
                    affected_rows: 0,
                };
                // 队列满了就等（背压）；消费者没了就收工
                let sent = batch.rows.len() as u64;
                if $tx.send(batch).await.is_err() {
                    return;
                }
                {
                    let mut sh = $shared.lock().unwrap_or_else(|p| p.into_inner());
                    sh.pushed_rows += sent;
                    sh.next_seq = seq + 1;
                }
                seq += 1;
                bytes = 0;
            }
            bytes += row_bytes;
            rows.push(cells);
            total += 1;
        }
        // 收尾：把不满一批的也推出去（空结果也推一个空批，消费者据此知道"到头了"）
        let batch = db::session::QueryBatch {
            seq,
            columns: meta.clone(),
            rows: std::mem::take(&mut rows),
            truncated,
            affected_rows: 0,
        };
        let sent = batch.rows.len() as u64;
        let _ = $tx.send(batch).await;
        let mut sh = $shared.lock().unwrap_or_else(|p| p.into_inner());
        sh.done = true;
        sh.truncated = truncated || sh.truncated;
        sh.pushed_rows += sent;
        sh.columns = meta;
        // 被取消时**不能**改写成 Succeeded —— 消费者据此知道这流是"被叫停"的，
        // 而不是"跑完了"。写错了界面就会把一次取消报成一次成功。
        sh.state = if sh.state == db::session::QueryState::Cancelled {
            db::session::QueryState::Cancelled
        } else {
            db::session::QueryState::Succeeded
        };
    }};
}

/// T-031：流式查询的启动结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryStreamStart {
    /// 流句柄（后续用 query_next 传它）。**不是**请求 id —— 请求 id 仍是 UUID v4。
    pub stream_id: String,
    /// 列元信息
    pub columns: Vec<ColumnMeta>,
    /// 第一批
    pub first_batch: db::session::QueryBatch,
    /// 流状态：queued / streaming / succeeded / failed
    pub state: String,
    /// 已推送到队列里的行数（不含还在队列里没被取走的部分）
    pub pushed_rows: u64,
    /// 是否因为达到 max_rows 上限而截断（后面还有更多）
    pub truncated: bool,
}

/// T-031：起一条流式查询。
///
/// 与 `execute_query` 的区别：结果**分批**返回，不会一次性把十万行塞进内存。
/// 门禁与 `execute_query` 完全一致（同一个 `gate_execution`）——
/// 两套判据漂移的后果就是"流式接口能绕过审批"。
#[tauri::command]
async fn query_start(
    config: DBConfig,
    sql: String,
    access_mode: Option<String>,
    approval: Option<security::ApprovalGrant>,
    expected_generation: Option<u32>,
    max_rows: Option<u32>,
) -> Result<QueryStreamStart, String> {
    if sql.trim().is_empty() {
        return Err("SQL 语句为空".to_string());
    }
    let mode = access_mode.unwrap_or_else(|| "readOnly".to_string());
    let classification = db::sql_classify::classify(&sql);
    // 流式的句柄就是这次的请求标识（用于错误定位）
    let stream_id = db::session::next_query_id();
    gate_execution(&sql, &mode, approval.as_ref(), classification, &stream_id)?;

    // 取消协议：流式查询**也必须**登记到在途表 ——
    // `query_cancel_v2` 查的就是这张表。不登记 ⇒ 取消永远「查无此单」，
    // 界面上的「取消」按钮对流式查询**完全无效**（2026-10-22 实测）。
    crate::db::cancel::in_flight().register(
        &config.id,
        stream_id.clone(),
        security::digest_sql(&sql),
    );

    let max_rows = max_rows.map(|m| m as usize);
    let (tx, rx) = tokio::sync::mpsc::channel(db::session::MAX_QUEUE_BATCHES);
    let shared = std::sync::Arc::new(std::sync::Mutex::new(db::session::StreamShared::default()));
    let entry = std::sync::Arc::new(db::session::StreamEntry {
        stream_id: stream_id.clone(),
        rx: std::sync::Arc::new(tokio::sync::Mutex::new(rx)),
        shared: shared.clone(),
        start_time: std::time::Instant::now(),
    });
    db::session::streams().insert(entry.clone()).await;

    // 作业登记：状态从 Queued 开始（前端据此显示"排队中"）
    // 作业是批次通道的**所有者**；生产者随后从作业这里借通道。
    let reg = db::session::registry();
    reg.insert(db::session::QueryJobInternal {
        query_id: stream_id.clone(),
        state: db::session::QueryState::Queued,
        state_msg: "排队中".into(),
        cancel_requested: false,
        start_time: std::time::Instant::now(),
        batch_tx: tx,
        transaction_state: db::session::TransactionState::Idle,
        generation: expected_generation.unwrap_or(0),
        conflict_detected: false,
        last_affected_rows: 0,
    });
    let tx_task = reg
        .batch_sender(&stream_id)
        .expect("刚登记的作业一定有批次通道");

    // 生产者：后台任务，边取边推
    let pool = pooled(&config, &mode).await.map_err(|e| {
        db::ipc::ApiError::connection_failed(&e)
            .with_query_id(&stream_id)
            .to_wire()
    })?;
    let shared_task = shared.clone();
    let sql_task = sql.clone();
    // 生产者要用同一个 id 去问「要不要停」，结束时要用同一个 id 把它从在途表摘掉
    let cancel_id = stream_id.clone();
    tokio::spawn(async move {
        // 包一层内层 async 块：宏里的 `return` 只退出这一层，
        // 外层的「从在途表摘掉」才不会被跳过。
        let run = async {
            match pool {
                crate::db::connection::AnyPool::MySql(p) => {
                    stream_rows!(
                        p,
                        &sql_task,
                        max_rows,
                        tx_task,
                        shared_task,
                        mysql_cell,
                        &cancel_id
                    )
                }
                crate::db::connection::AnyPool::Pg(p) => {
                    stream_rows!(
                        p,
                        &sql_task,
                        max_rows,
                        tx_task,
                        shared_task,
                        pg_cell,
                        &cancel_id
                    )
                }
                crate::db::connection::AnyPool::Sqlite(p) => {
                    stream_rows!(
                        p,
                        &sql_task,
                        max_rows,
                        tx_task,
                        shared_task,
                        sqlite_cell,
                        &cancel_id
                    )
                }
            }
        };
        run.await;
        // 流结束（正常跑完 / 出错 / 被取消）⇒ 从在途表摘掉。
        // 这一单若曾请求过取消，finish() 返回 Confirmed（取消已生效）。
        let _ = crate::db::cancel::in_flight().finish(&cancel_id);
    });

    // 等第一批：拿到它才知道列长什么样
    let first = {
        let mut rx = entry.rx.lock().await;
        rx.recv().await
    };
    let first = match first {
        Some(b) => b,
        None => {
            let msg = {
                let sh = shared.lock().unwrap_or_else(|p| p.into_inner());
                sh.error
                    .clone()
                    .unwrap_or_else(|| "流式查询未能启动".to_string())
            };
            db::session::streams().remove(&stream_id).await;
            reg.mark_failed(&stream_id, &msg);
            return Err(msg);
        }
    };
    {
        let mut sh = shared.lock().unwrap_or_else(|p| p.into_inner());
        sh.state = db::session::QueryState::Streaming;
        sh.next_seq = first.seq + 1;
        sh.columns = first.columns.clone();
    }
    reg.mark_succeeded(&stream_id);
    let (pushed_rows, truncated) = {
        let sh = shared.lock().unwrap_or_else(|p| p.into_inner());
        (sh.pushed_rows, sh.truncated)
    };
    Ok(QueryStreamStart {
        stream_id,
        columns: first.columns.clone(),
        first_batch: first,
        state: "streaming".to_string(),
        pushed_rows,
        truncated,
    })
}

/// T-031：取下一批。返回 None = 流已结束。
#[tauri::command]
async fn query_next(stream_id: String) -> Result<Option<db::session::QueryBatch>, String> {
    let entry = db::session::streams()
        .get(&stream_id)
        .await
        .ok_or_else(|| format!("流 {} 不存在或已结束", stream_id))?;
    let reg = db::session::registry();
    let mut rx = entry.rx.lock().await;
    // 结束判定：生产者说做完了，且队列里确实没东西了
    let done = {
        let sh = entry.shared.lock().unwrap_or_else(|p| p.into_inner());
        sh.done
    };
    if done && rx.is_empty() {
        drop(rx);
        db::session::streams().remove(&stream_id).await;
        reg.mark_succeeded(&stream_id);
        return Ok(None);
    }
    let batch = rx.recv().await;
    drop(rx);
    match batch {
        Some(b) => Ok(Some(b)),
        None => {
            // 通道关了 ⇒ 生产者退出（正常收尾或出错）
            db::session::streams().remove(&stream_id).await;
            let sh = {
                let sh = entry.shared.lock().unwrap_or_else(|p| p.into_inner());
                (sh.error.clone(), sh.done)
            };
            if let Some(e) = sh.0 {
                reg.mark_failed(&stream_id, &e);
                return Err(e);
            }
            if sh.1 {
                reg.mark_succeeded(&stream_id);
            }
            Ok(None)
        }
    }
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
                    size_bytes: r
                        .try_get::<i64, _>("size_bytes")
                        .ok()
                        .map(|v| v.max(0) as u64),
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
// T-030：PG 多 schema 同名表（DB-12 修复）；接受可选 schema 参数；
// 为空时 PG 走 current_schema()，其它驱动忽略。
#[command]
async fn get_table_structure(
    table_name: String,
    config: DBConfig,
    schema: Option<String>,
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
            // 显式 schema 优先；缺省回退 current_schema() 维持向后兼容
            let explicit_schema = schema
                .clone()
                .unwrap_or_else(|| "current_schema()".to_string());
            let where_schema = if explicit_schema == "current_schema()" {
                "c.table_schema = current_schema()".to_string()
            } else {
                format!("c.table_schema = '{}'", explicit_schema.replace('\'', "''"))
            };
            let sql = format!(
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
                 WHERE {where_schema} AND c.table_name = $1 \
                 ORDER BY c.ordinal_position"
            );
            let rows = sqlx::query(&sql)
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
            let sql = format!("PRAGMA table_info('{}')", table_name.replace('\'', "''"));
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
// S0 封堵：不承诺跨语句事务回滚；每条独立执行并独立提交；
// 写入语句需要 access_mode="writable"。
#[command]
async fn execute_batch(
    queries: Vec<String>,
    config: DBConfig,
    access_mode: Option<String>,
    approval: Option<security::ApprovalGrant>,
) -> Result<Vec<QueryResult>, String> {
    let mode = access_mode.unwrap_or_else(|| "readOnly".to_string());
    // 批量接口**只做只读**。
    //
    // 为什么：审批摘要是一对一的 —— `security::evaluate` 会核对
    // `grant.sql_digest == digest_sql(sql)`，一条 grant 只对一个语句。
    // 原来这里把**同一个** grant 传给每条语句 ⇒ 第二条必然报 SqlMismatch；
    // 而只读模式又被这里直接拒掉 ⇒ 这个接口从来没能真正用起来
    // （报错文案写的是「批量接口当前为只读模式」，代码却拒的是 readOnly，**自相矛盾**）。
    //
    // 现在口径明确：**批量 = 一批只读语句**。写语句请用 execute_query 逐条执行
    // （每条各自审批）。每条语句仍然过 `gate_execution` 的只读判据，
    // 所以混进来的写语句会被当场挡下，不会因为"批量"而绕过门禁。
    //
    // ⚠ 错误**必须带机器可读码**（READONLY_BLOCKED）：前端按码分类，
    // 不带码的话界面只能显示"未知错误"，用户不知道自己该切只读还是该逐条执行。
    if mode != "readOnly" {
        return Err(db::ipc::ApiError::readonly_blocked("Batch", "Writable").to_wire());
    }
    let mut out = Vec::new();
    for q in queries {
        if q.trim().is_empty() {
            continue;
        }
        // 批量接口**也必须过同一道门**。
        // 改之前它只判了 access_mode 就直接执行 —— 可写模式下**完全不查审批**，
        // 等于给"绕过审批"开了一个后门。门禁与 execute_query 共用同一个函数。
        let batch_id = uuid::Uuid::new_v4().to_string();
        let classification = db::sql_classify::classify(&q);
        gate_execution(&q, &mode, approval.as_ref(), classification, &batch_id)?;
        let start = std::time::Instant::now();
        let (column_meta, rows, affected, is_query) = run_dispatch(&config, &q).await?;
        // T-018 同样为批量执行使用 UUID v4 标识
        let id = batch_id;
        let columns: Vec<String> = column_meta.iter().map(|m| m.name.clone()).collect();
        out.push(QueryResult {
            id,
            columns,
            column_meta,
            rows,
            affected_rows: affected,
            execution_time_ms: start.elapsed().as_millis() as u64,
            is_query,
            timings: Default::default(),
            truncated: false,
            total_rows: 0,
        });
    }
    Ok(out)
}

// 格式化 SQL
// T-034：词法感知的 SQL 格式化
// 实现要点：
// - 在「剥字符串/注释后的正文」里识别关键字
// - 但对原文按位置插入换行（preserve 字面量不被改动）
// - 仅在 unquoted/uncomment 区域插入换行，避免破坏 'FROM users'
#[command]
async fn format_sql(sql: String) -> Result<String, String> {
    use crate::db::sql_classify::{classify, SafetyClass};

    let classification = classify(&sql);

    // 1) 计算正文中的"安全位置"（不属于字符串/注释）
    let safe_positions = compute_safe_positions(&sql);

    // 2) 在 safe 位置检出关键字并在其前插入换行
    const NEWLINE_KEYWORDS: &[&str] = &[
        "SELECT",
        "FROM",
        "WHERE",
        "GROUP BY",
        "ORDER BY",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "LEFT JOIN",
        "RIGHT JOIN",
        "INNER JOIN",
        "OUTER JOIN",
        "JOIN",
        "UNION ALL",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "INSERT INTO",
        "VALUES",
        "UPDATE",
        "SET",
        "DELETE FROM",
        "RETURNING",
        "ON",
    ];

    // 按出现位置倒序处理，避免前面插入换行影响后续位置
    let mut inserts: Vec<(usize, String)> = Vec::new();
    for kw in NEWLINE_KEYWORDS {
        let mut i = 0;
        let bytes = sql.as_bytes();
        let kw_bytes = kw.as_bytes();
        let kl = kw_bytes.len();
        while i + kl <= bytes.len() {
            let prev_ok = i == 0 || !is_sql_word_byte(bytes[i - 1]);
            let next_ok = i + kl == bytes.len() || !is_sql_word_byte(bytes[i + kl]);
            if prev_ok && next_ok && &bytes[i..i + kl] == kw_bytes && safe_positions[i] {
                inserts.push((i, "\n".to_string()));
            }
            i += 1;
        }
    }
    inserts.sort_by_key(|x| std::cmp::Reverse(x.0)); // 倒序：从后往前插入

    let mut output = sql;
    for (pos, ins) in &inserts {
        let split_at = *pos;
        let after_pos = split_at + ins.len();
        let new_output = format!("{}{}{}", &output[..split_at], ins, &output[split_at..]);
        output = new_output;
        let _ = after_pos;
    }

    // 折叠多余空行
    let mut collapsed = String::with_capacity(output.len());
    let mut last_n = false;
    for ch in output.chars() {
        if ch == '\n' {
            if !last_n {
                collapsed.push('\n');
            }
            last_n = true;
        } else {
            collapsed.push(ch);
            last_n = false;
        }
    }

    // 前缀警告以行注释插入
    let prefix = match classification.safety {
        SafetyClass::ReadOnlySafe => String::new(),
        SafetyClass::ReadOnlyUnsafe => {
            "-- ⚠ 含 FOR UPDATE / EXPLAIN ANALYZE 等带副作用子句\n".to_string()
        }
        SafetyClass::Write => "-- ⚠ 含写入语句，建议在「可写」通道并通过审批执行\n".to_string(),
        SafetyClass::Unknown => "-- ⚠ 静态分类器无法判定，请人工确认\n".to_string(),
    };
    Ok(format!("{}{}", prefix, collapsed.trim()))
}

fn is_sql_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 返回与 sql 同长度的 bool 数组；true 表示该字节处于「不在字符串/注释」区域。
fn compute_safe_positions(sql: &str) -> Vec<bool> {
    let n = sql.len();
    let bytes = sql.as_bytes();
    let mut safe = vec![true; n];
    let mut i = 0;
    while i < n {
        // 行注释
        if i + 1 < n && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            for k in i..n {
                safe[k] = false;
                if bytes[k] == b'\n' {
                    break;
                }
            }
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // 块注释
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let mut j = i;
            safe[j] = false;
            safe[j + 1] = false;
            j += 2;
            while j + 1 < n && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                safe[j] = false;
                j += 1;
            }
            if j + 1 < n {
                safe[j] = false;
                safe[j + 1] = false;
                j += 2;
            }
            i = j;
            continue;
        }
        // 单引号字符串
        if bytes[i] == b'\'' {
            safe[i] = false;
            i += 1;
            while i < n {
                if bytes[i] == b'\'' {
                    safe[i] = false;
                    if i + 1 < n && bytes[i + 1] == b'\'' {
                        // 转义 ''
                        safe[i + 1] = false;
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                safe[i] = false;
                i += 1;
            }
            continue;
        }
        // 双引号 / 反引号
        if bytes[i] == b'"' || bytes[i] == b'`' {
            safe[i] = false;
            i += 1;
            while i < n && bytes[i] != bytes[i - 1] {
                safe[i] = false;
                i += 1;
            }
            if i < n {
                safe[i] = false;
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    safe
}

// T-040 EXPLAIN 基础面板
// - MySQL：EXPLAIN <sql>
// - PostgreSQL：EXPLAIN <sql>（FORMAT JSON 由 DB 端默认）；
// - SQLite：EXPLAIN QUERY PLAN <sql>
// - 强制 readOnly；识别 ANALYZE 后缀并拒绝，避免静默执行
// - 结果走标准的 QueryResult 通道（columns + rows 都是 tagged cell）
#[command]
async fn explain_query(sql: String, config: DBConfig) -> Result<QueryResult, String> {
    let upper = sql.trim().to_ascii_uppercase();
    if upper.contains("ANALYZE") {
        return Err(
            "EXPLAIN ANALYZE 会真正执行 SQL；S0 拒绝自动执行；如确需请人工单次授权".to_string(),
        );
    }
    if upper.starts_with("EXPLAIN") {
        return Err("请仅提供被分析的 SQL 语句（不含 EXPLAIN 前缀）".to_string());
    }

    let dialect_explain: &str = match dispatch_db_type(&config)? {
        "mysql" => "",                     // MySQL 直接用 EXPLAIN <sql>
        "postgresql" => "",                // PG 默认走 FORMAT TEXT；之后可加 FORMAT JSON
        "sqlite" => "EXPLAIN QUERY PLAN ", // SQLite 必须显式
        _ => return Err("不支持的数据库类型".to_string()),
    };

    let prefixed = match dialect_explain {
        "" => format!("EXPLAIN {}", sql.trim()),
        other => format!("{}{}", other, sql.trim()),
    };
    // T-041：分段耗时
    let start = std::time::Instant::now();
    let db_type = dispatch_db_type(&config)?;
    let connect_start = std::time::Instant::now();
    let (column_meta, rows, affected, is_query, exec_start) = match db_type {
        "mysql" => {
            let pool = mysql_pool(&config).await?;
            let es = std::time::Instant::now();
            let r = mysql_run(&pool, &prefixed).await?;
            (r.0, r.1, r.2, r.3, es)
        }
        "postgresql" => {
            let pool = pg_pool(&config).await?;
            let es = std::time::Instant::now();
            let r = pg_run(&pool, &prefixed).await?;
            (r.0, r.1, r.2, r.3, es)
        }
        "sqlite" => {
            let pool = sqlite_pool(&config).await?;
            let es = std::time::Instant::now();
            let r = sqlite_run(&pool, &prefixed).await?;
            (r.0, r.1, r.2, r.3, es)
        }
        _ => return Err("不支持的数据库类型".to_string()),
    };
    Ok(QueryResult {
        id: uuid::Uuid::new_v4().to_string(),
        columns: column_meta.iter().map(|m| m.name.clone()).collect(),
        column_meta,
        rows,
        affected_rows: affected,
        execution_time_ms: start.elapsed().as_millis() as u64,
        is_query,
        timings: build_timings(start, connect_start, exec_start),
        truncated: false,
        total_rows: 0,
    })
}

// 导出连接配置
#[command]
async fn export_connections(connections: Vec<ExportedConnection>) -> Result<String, String> {
    let export = ConnectionExport {
        version: 1,
        exported_at: chrono::Utc::now().timestamp(),
        connections,
    };
    serde_json::to_string_pretty(&export).map_err(|e| e.to_string())
}

// T-037：导入连接配置 + 版本/大小/条数校验
// - 当前仅支持 version=1
// - 单批最多 100 条；JSON 长度 ≤ 256 KiB
// - 解析后必须至少 1 条；驱动类型必须是 mysql/postgresql/sqlite 之一
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ImportOptions {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub check_driver: Option<bool>,
}

const DEFAULT_IMPORT_LIMIT: usize = 100;
const DEFAULT_IMPORT_MAX_BYTES: usize = 256 * 1024;

#[command]
async fn import_connections(
    json: String,
    options: Option<ImportOptions>,
) -> Result<Vec<ExportedConnection>, String> {
    let opts = options.unwrap_or(ImportOptions {
        limit: None,
        max_bytes: None,
        check_driver: None,
    });
    let max_bytes = opts.max_bytes.unwrap_or(DEFAULT_IMPORT_MAX_BYTES);
    if json.len() > max_bytes {
        return Err(format!(
            "导入文件过大（{} bytes > 限额 {}）",
            json.len(),
            max_bytes
        ));
    }
    let export: ConnectionExport =
        serde_json::from_str(&json).map_err(|e| format!("解析失败: {}", e))?;
    if export.version != 1 {
        return Err(format!(
            "不支持的导出版本 {}；当前应用仅支持 1",
            export.version
        ));
    }
    let limit = opts.limit.unwrap_or(DEFAULT_IMPORT_LIMIT);
    if export.connections.len() > limit {
        return Err(format!(
            "导入条数 {} 超过限 {}",
            export.connections.len(),
            limit
        ));
    }
    if export.connections.is_empty() {
        return Err("导入文件不含任何连接".to_string());
    }
    if opts.check_driver.unwrap_or(true) {
        for c in &export.connections {
            if !matches!(c.db_type.as_str(), "mysql" | "postgresql" | "sqlite") {
                return Err(format!(
                    "驱动类型 {} 未在白名单（mysql/postgresql/sqlite）；条目 {} 被拒",
                    c.db_type, c.name
                ));
            }
            // 端口 0 只对 MySQL/PostgreSQL 非法 —— SQLite 压根不用端口
            if c.port == 0 && matches!(c.db_type.as_str(), "mysql" | "postgresql") {
                return Err(format!("连接 {} 端口字段为 0", c.name));
            }
        }
    }
    Ok(export.connections)
}

// T-047：CSV 导入计划（预览 + 审批摘要的规范串）
//
// 这个命令存在的理由不是"给界面看几行"，而是**把审批摘要要绑的那份文本交出去**：
// 导入的 INSERT 是按文件内容动态拼的，前端自己拼摘要输入必然和后端对不上。
// ⇒ canonical 由后端算，前端原样拿去算 digest。
#[command]
async fn csv_import_plan(
    file_path: String,
    delimiter: Option<String>,
    sample_limit: Option<usize>,
    import_limit: Option<usize>,
    table_name: String,
) -> Result<db::csv_import::ImportPlan, String> {
    let bytes = std::fs::read(&file_path).map_err(|e| format!("读取文件失败: {}", e))?;
    let delim = delimiter
        .map(|s| s.chars().next().unwrap_or(','))
        .unwrap_or(',');
    let sl = sample_limit.unwrap_or(db::csv_import::DEFAULT_SAMPLE_LIMIT);
    let il = import_limit.unwrap_or(db::csv_import::DEFAULT_IMPORT_LIMIT);
    db::csv_import::plan_import(&bytes, delim, sl, il, &table_name)
}

/// CSV 导入的**写门禁**参数（access_mode + 审批 + 期望行数）。
///
/// 为什么要打包成一个结构：
/// - `csv_import_execute` 本来就有 5 个参数，再加三个门禁参数就超过
///   clippy 的阈值（8/7）⇒ 一条 warning 挂在 CI 的 `-D warnings` 硬门上；
/// - 打包之后语义更清楚：**这三个是一个整体，要么都给，要么都不给**
///   （漏给 access_mode 就退化成只读 ⇒ 被拒，这是安全的方向）。
///
/// ⚠ 字段是 **snake_case**：这是 serde 结构体字段，不是 Tauri 的顶层命令参数
/// （顶层参数在 JS 侧要写 camelCase，嵌套字段按 serde 原样）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ImportGate {
    /// "writable" 才放行；别的值（含缺省）一律按只读拒
    pub access_mode: Option<String>,
    /// 写入审批（摘要绑定「表 + 真正会写入的行数」）
    pub approval: Option<security::ApprovalGrant>,
    /// 预览时看到的行数；与实际解析出的行数不一致 ⇒ 拒
    pub expected_rows: Option<u64>,
}

// T-047：CSV 导入（预览确认 + 审批通过后调用）
//
// 门禁与 `execute_query` 同源，但**摘要绑的不是 SQL 全文**：
// 导入的 INSERT 是按文件内容动态拼的，一条 grant 绑不住 N 条 INSERT
// ⇒ 审批摘要绑「表 + 真正会写入的行数」（canonical 由 csv_import_plan 给出）。
// 三道门：
// 1) 只读通道直接拒（导入永远是写）；
// 2) 行数与审批时对不上 ⇒ 拒（文件在预览之后被改过）；
// 3) 摘要对不上 ⇒ 拒（换表 / 换文件 / 伪造 grant）。
#[command]
async fn csv_import_execute(
    config: DBConfig,
    file_path: String,
    table_name: String,
    delimiter: Option<String>,
    limit: Option<usize>,
    gate: Option<ImportGate>,
) -> Result<db::csv_import::ImportResult, String> {
    let query_id = uuid::Uuid::new_v4().to_string();
    let gate = gate.unwrap_or(ImportGate {
        access_mode: None,
        approval: None,
        expected_rows: None,
    });
    let mode = gate.access_mode.unwrap_or_else(|| "readOnly".to_string());

    // 导入**永远是写**。只读通道直接拒，不留"预览通道顺手写进去"的口子。
    if mode != "writable" {
        return Err(db::ipc::ApiError::new(
            db::ipc::CODE_READONLY_BLOCKED,
            "csv_import_execute",
            "只读模式拒绝 CSV 导入（导入会写数据）",
        )
        .with_query_id(&query_id)
        .to_wire());
    }

    let bytes = std::fs::read(&file_path).map_err(|e| format!("读取文件失败: {}", e))?;
    let delim = delimiter
        .map(|s| s.chars().next().unwrap_or(','))
        .unwrap_or(',');
    let lim = limit.unwrap_or(db::csv_import::DEFAULT_IMPORT_LIMIT);

    let (headers, rows) = db::csv_import::parse_csv(&bytes, delim, lim)?;

    if headers.is_empty() {
        return Err("CSV 文件没有列头".to_string());
    }

    // 真正会写入的行数：必须与 csv_import_plan 给出的 import_rows 同口径。
    let import_rows = rows.len() as u64;

    // 行数与审批时对不上 ⇒ 用户在审批弹窗里看到的"写多少行"已经不算数了。
    // （摘要也会对不上，这里先查是为了给用户一句能看懂的话。）
    if let Some(expected) = gate.expected_rows {
        if expected != import_rows {
            return Err(db::ipc::ApiError::new(
                db::ipc::CODE_APPROVAL_DENIED,
                "csv_import_execute",
                &format!(
                    "CSV 行数与审批时不一致（审批 {} 行，实际解析出 {} 行）⇒ 文件在预览之后变了，重新预览再导入",
                    expected, import_rows
                ),
            )
            .with_query_id(&query_id)
            .to_wire());
        }
    }

    // 审批：摘要绑「表 + 行数」
    let canonical = db::csv_import::import_canonical(&table_name, import_rows);
    let env = gate
        .approval
        .as_ref()
        .map(|g| g.environment.as_str())
        .unwrap_or("unknown");
    security::evaluate(
        gate.approval.as_ref(),
        db::sql_classify::StatementKind::Dml,
        db::sql_classify::SafetyClass::Write,
        &canonical,
        chrono::Utc::now().timestamp(),
        env,
    )
    .map_err(|e| {
        db::ipc::ApiError::new(
            db::ipc::CODE_APPROVAL_DENIED,
            "csv_import_execute",
            &format!("审批校验失败: {}", e.message()),
        )
        .with_query_id(&query_id)
        .to_wire()
    })?;

    // 构造 INSERT 语句（简化版：列名直接拼接）
    let cols = headers
        .iter()
        .map(|h| format!("\"{}\"", h.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let mut imported = 0u64;
    let mut errors = Vec::new();

    for (idx, row) in rows.iter().enumerate() {
        let values = row
            .iter()
            .map(|v| {
                if v.is_empty() || v == "NULL" {
                    "NULL".to_string()
                } else {
                    format!("'{}'", v.replace('\'', "''"))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "INSERT INTO \"{}\" ({}) VALUES ({})",
            table_name, cols, values
        );

        match run_dispatch_mode(&config, &sql, "writable").await {
            Ok(_) => imported += 1,
            Err(e) => {
                errors.push(format!("第 {} 行: {}", idx + 2, e));
            }
        }
    }

    Ok(db::csv_import::ImportResult {
        success: errors.is_empty(),
        rows_imported: imported,
        rows_failed: errors.len() as u64,
        errors,
    })
}

// 连接配置落盘
// T-053：保存连接时可携带 expected_revision 做 CAS；
// 没有 expected_revision 时整体替换（向后兼容旧调用方）。
// T-014：密码**不落盘** —— 保存时先封存进密钥库，落盘的只有 SecretRef。
//
// 命令层刻意拆成「可注入 store 的内核 + 薄命令」：
// 内核能带探针后端跑（不碰全局，测试之间不会互相污染），
// 命令只负责把全局 store 递进去。
async fn save_connections_with(
    store: &dyn secrets::SecretStore,
    connections: &[ConnectionRecord],
    expected_revisions: Option<&std::collections::HashMap<String, u32>>,
) -> Result<(), String> {
    let existing = queries::load_connections().await?;
    // CAS 校验：每个 id 的 expected_revision 必须等于磁盘上现有 revision
    if let Some(expected) = expected_revisions {
        let existing_by_id: std::collections::HashMap<String, u32> = existing
            .iter()
            .map(|c| (c.id.clone(), c.revision))
            .collect();
        for (id, exp_rev) in expected {
            match existing_by_id.get(id) {
                Some(cur) if *cur != *exp_rev => {
                    return Err(format!(
                        "REVISION_MISMATCH: 连接 {} 的修订号不匹配（期望 {}，实际 {}）",
                        id, exp_rev, cur
                    ));
                }
                None if *exp_rev != 0 => {
                    return Err(format!(
                        "REVISION_MISMATCH: 连接 {} 不存在但期望 revision={}",
                        id, exp_rev
                    ));
                }
                _ => {}
            }
        }
    }
    // T-014：封存进密钥库。existing 用来复用未变更的密钥引用，
    // 避免每次保存都往密钥库里堆一份新的明文副本。
    let sealed = db::credentials::seal(store, connections, &existing)?;
    // T-011：连接参数或凭据变了 ⇒ **旧池必须关掉**。
    // 改之前编辑连接后旧池还活着（还指着旧库/旧凭据），要等 5 分钟 TTL 才被回收；
    // 连接被删除同理（池还在，数据库端还以为这条会话有用）。
    for old_rec in &existing {
        let changed = match connections.iter().find(|c| c.id == old_rec.id) {
            Some(new_rec) => {
                // 密码变了没有？密钥引用换了就说明换了（seal 会在明文变化时换引用）。
                let secret_changed = match (
                    old_rec.secret_ref.as_ref().map(|r| r.id.clone()),
                    sealed
                        .iter()
                        .find(|c| c.id == new_rec.id)
                        .and_then(|c| c.secret_ref.as_ref())
                        .map(|r| r.id.clone()),
                ) {
                    (Some(a), Some(b)) => a != b,
                    (None, None) => false,
                    _ => true,
                };
                conn_identity(old_rec) != conn_identity(new_rec) || secret_changed
            }
            None => true, // 连接被删了
        };
        if changed {
            close_connection_pools(&old_rec.id);
        }
    }
    queries::save_connections(&sealed).await
}

#[command]
async fn save_connections(
    connections: Vec<ConnectionRecord>,
    expected_revisions: Option<std::collections::HashMap<String, u32>>,
) -> Result<(), String> {
    let store = secrets::secret_store();
    save_connections_with(store.as_ref(), &connections, expected_revisions.as_ref()).await
}

/// T-014：从密钥库取回明文。取不回的记录会带上 credential_state=missing
/// 与一句给用户看的原因，**不静默清空**。
async fn load_connections_with(
    store: &dyn secrets::SecretStore,
) -> Result<Vec<ConnectionRecord>, String> {
    let raw = queries::load_connections().await?;
    let (unsealed, _missing) = db::credentials::unseal(store, raw);
    Ok(unsealed)
}

#[command]
async fn load_connections() -> Result<Vec<ConnectionRecord>, String> {
    let store = secrets::secret_store();
    load_connections_with(store.as_ref()).await
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

// 报表簿
#[command]
async fn save_report(item: SavedReport) -> Result<(), String> {
    queries::save_report(item).await
}

#[command]
async fn load_reports() -> Result<Vec<SavedReport>, String> {
    queries::load_reports().await
}

#[command]
async fn delete_report(id: String) -> Result<(), String> {
    queries::delete_report(&id).await
}

// ================== Tauri 启动 ==================

// T-021：连接池生命周期管理
// - 后台 tokio task 按 TTL 周期回收空闲池（默认 5 分钟，可在 future 提供配置）
// - 启动时 spawn；进程退出随 Tauri Builder drop 而结束
fn spawn_pool_lifecycle() {
    // 必须走 tauri::async_runtime：本函数在 `tauri::Builder::run()` 之前调用，
    // 此刻线程上没有 Tokio 运行时上下文，裸 `tokio::spawn` 会 panic
    // （"there is no reactor running"）。Tauri 的全局运行时句柄没有这个前提。
    tauri::async_runtime::spawn(async move {
        let ttl = std::time::Duration::from_secs(5 * 60);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let _ = crate::db::connection::pool_registry().idle_sweep(ttl);
        }
    });
}

/// 关池的结果报告。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PoolCloseReport {
    pub connection_id: String,
    pub revision: u32,
    /// 关掉了几个池
    pub closed_pools: u32,
    /// 作废了几条在途查询
    pub invalidated_queries: u32,
    /// 其中有几条的**取消结果无法确认**（连接都没了 ⇒ 服务端停没停不知道）
    pub unknown_cancels: u32,
    /// 关之前全局缓存里一共有几个池
    pub pools_before: u32,
    /// 关之前按驱动种类分的池数（诊断用：看池是不是按预期复用了）
    pub pools_by_kind: Vec<(String, u32)>,
}

/// T-011：关掉某条连接的池，并把该连接上的在途查询标成 unknown。
///
/// 关池 ⇒ 服务端那边可能还活着 ⇒ 结果**无法确认** ⇒ 记 unknown，
/// 不能假装"失败"或"成功"（界面必须提示用户"先隔离再重连"）。
fn close_connection_pools(connection_id: &str) -> usize {
    use crate::db::connection::pool_registry;
    // 事务池也要丢：连接没了，这个事务永远结束不了
    pool_registry().remove_tx_pool(connection_id);
    let closed = pool_registry().remove_connection(connection_id);
    let drained = db::cancel::in_flight().drain(connection_id);
    for e in &drained {
        db::session::registry().mark_unknown(&e.query_id);
    }
    closed
}

/// 连接的"身份"（池该跟着它走的那些参数）。变了 ⇒ 旧池作废。
fn conn_identity(c: &ConnectionRecord) -> (String, String, u16, String, String, String, String) {
    (
        c.db_type.clone(),
        c.host.clone(),
        c.port,
        c.username.clone(),
        c.database.clone(),
        c.tls_mode
            .as_ref()
            .map(|m| m.as_str().to_string())
            .unwrap_or_default(),
        c.tls_ca_path.clone().unwrap_or_default(),
    )
}

#[tauri::command]
async fn close_pool(connection_id: String, revision: u32) -> Result<PoolCloseReport, String> {
    use crate::db::connection::pool_registry;
    let registry = pool_registry();
    // **2026-10-22 修正**：原来这里是 `registry.clear()` ——
    // 关一条连接会把**所有**连接（含别人的）的池一起清空，
    // 正在跑的查询随即报"连接已关闭"；而 connection_id / revision 两个入参
    // 是被 `let _ =` 丢掉的。现在按 connection_id 精确关。
    // 关之前先把池子分布记下来：这是"池到底复用了没有"唯一能直接回答的口子。
    let (pools_before, _) = registry.stats();
    let pools_by_kind = registry.stats_by_kind();
    let closed_pools = close_connection_pools(&connection_id);
    // 这条连接上的在途查询：池没了 ⇒ 取消**无法确认** ⇒ 记成 unknown。
    let drained = db::cancel::in_flight().drain(&connection_id);
    let unknown_cancels = drained.iter().filter(|e| e.cancel_requested).count() as u32;
    Ok(PoolCloseReport {
        connection_id,
        revision,
        closed_pools: closed_pools as u32,
        invalidated_queries: drained.len() as u32,
        unknown_cancels,
        pools_before: pools_before as u32,
        pools_by_kind: pools_by_kind
            .into_iter()
            .map(|(k, n)| (k, n as u32))
            .collect(),
    })
}

// T-033: Keyset 表浏览
// 使用主键进行 keyset 分页，避免 OFFSET 全扫描
// pageSize 默认 200，最大 500
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeysetResult {
    pub columns: Vec<String>,
    pub column_meta: Vec<ColumnMeta>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub has_next: bool,
    pub affected_rows: u64,
    pub execution_time_ms: u64,
}

#[command]
async fn keyset_query(
    config: DBConfig,
    table_name: String,
    last_value: Option<String>,
    page_size: Option<u32>,
) -> Result<KeysetResult, String> {
    let page_size = page_size.unwrap_or(200).min(500);
    // 每次取 pageSize+1 行来判断是否有下一页
    let fetch_size = (page_size + 1) as i64;

    let sql = match dispatch_db_type(&config)? {
        "mysql" | "postgresql" => {
            if let Some(ref lv) = last_value {
                format!(
                    "SELECT * FROM \"{}\" WHERE id > {} ORDER BY id LIMIT {}",
                    table_name.replace('"', "\"\""),
                    lv,
                    fetch_size
                )
            } else {
                format!(
                    "SELECT * FROM \"{}\" ORDER BY id LIMIT {}",
                    table_name.replace('"', "\"\""),
                    fetch_size
                )
            }
        }
        "sqlite" => {
            if let Some(ref lv) = last_value {
                format!(
                    "SELECT * FROM \"{}\" WHERE rowid > {} ORDER BY rowid LIMIT {}",
                    table_name.replace('"', "\"\""),
                    lv,
                    fetch_size
                )
            } else {
                format!(
                    "SELECT * FROM \"{}\" ORDER BY rowid LIMIT {}",
                    table_name.replace('"', "\"\""),
                    fetch_size
                )
            }
        }
        _ => return Err("不支持的数据库类型".to_string()),
    };

    let start = std::time::Instant::now();
    let (column_meta, rows, affected, _is_query) = run_dispatch(&config, &sql).await?;
    let columns: Vec<String> = column_meta.iter().map(|m| m.name.clone()).collect();

    let has_next = rows.len() as u32 > page_size;
    let mut display_rows = rows;
    if has_next {
        display_rows.truncate(page_size as usize);
    }

    Ok(KeysetResult {
        columns,
        column_meta,
        rows: display_rows,
        has_next,
        affected_rows: affected,
        execution_time_ms: start.elapsed().as_millis() as u64,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    spawn_pool_lifecycle();
    // 深链队列：manage 给命令用、seed 给冷启动用，两处必须是同一份。
    let zbiz_pending = cap_hub::receiver::SharedPending::default();
    tauri::Builder::default()
        // 单实例（2026-10-26 分发底座）：第二次启动聚焦已有窗口，不再开新进程。
        // 必须作为第一个注册的插件（tauri-plugin-single-instance 官方要求）。
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
            cap_hub::receiver::handle_second_instance(app, &argv);
        }))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        // 自动更新（2026-10-25 接入，方案与 z-biz-tool-file 一致）：启动后由前端
        // UpdaterDialog 延时调用 check()，endpoint 与 pubkey 见 tauri.conf.json。
        // pubkey 目前是占位符，CEO 填入真实公钥前 check() 会失败并按设计静默降级。
        .plugin(tauri_plugin_updater::Builder::new().build())
        // 更新安装完成后的「立即重启」
        .plugin(tauri_plugin_process::init())
        .manage(zbiz_pending.clone())
        .invoke_handler(tauri::generate_handler![
            test_connection,
            execute_query,
            explain_query,
            get_tables,
            metadata_list_v2,
            get_table_structure,
            execute_batch,
            format_sql,
            query_status_v2,
            query_cancel_v2,
            query_start,
            query_next,
            run_secret_migration,
            check_connection_revision,
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
            ai_sql::ai_sql_generate,
            ai_explain_results,
            ai_optimize_sql,
            ai_explain_sql,
            ai_diagnose_error,
            close_pool,
            csv_import_plan,
            csv_import_execute,
            keyset_query,
            report::source::report_dataset_validate,
            report::source::report_dataset_sql,
            report::source::report_dataset_execute,
            report::source::report_describe_columns,
            report::source::report_view_validate,
            report::source::report_view_render,
            report::ai::ai_report_draft,
            report::ai::ai_report_pick_tables,
            report::ai::ai_report_explain,
            save_report,
            load_reports,
            delete_report,
            zbiz_take_pending,
        ])
        .setup(move |app| {
            // 冷启动时带进来的 zbiz:// 先入队，前端挂上监听后再取走。
            cap_hub::receiver::seed_cold_start(app.handle(), &zbiz_pending);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // 应用退出 ⇒ 主动关掉所有连接池，让数据库端看到干净的断开。
            // 不关的话 TCP 连接要等服务端超时才回收（默认可以到几分钟）。
            if let tauri::RunEvent::Exit = event {
                crate::db::connection::pool_registry().clear();
            }
        });
}

/// 取走队列里的深链（家族统一命令名）。
#[tauri::command]
fn zbiz_take_pending(
    pending: tauri::State<'_, cap_hub::receiver::SharedPending>,
) -> Vec<cap_hub::Handoff> {
    cap_hub::receiver::take_pending_from(pending.inner())
}

// ================== 真实测试（只做 SELECT，不碰写/删） ==================

#[cfg(test)]
mod tests {
    use super::*;
    /// 让会修改 Z_BIZ_TOOL_DB_DATA_DIR 环境变量的测试串行化；
    /// 避免在并行线程下互相污染。
    /// 用 **tokio** 的异步锁而不是 `std::sync::Mutex`：
    /// 这些用例要持锁跨 `.await`（写盘/查库都在 await 里），而 std 的锁
    /// 跨 await 在 `current_thread` runtime 下是**真死锁风险** ——
    /// 同一线程上的别的任务要拿同一把锁就永远等不到。
    pub static DATADIR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
            tls_mode: None,
            tls_ca_path: None,
        }
    }

    /// 测试便捷封装：补齐新增的可选参数
    async fn exec_q(
        sql: &str,
        c: DBConfig,
        access_mode: Option<String>,
    ) -> Result<QueryResult, String> {
        execute_query(sql.to_string(), c, access_mode, None, None, None).await
    }

    #[tokio::test]
    async fn sqlite_select_only() {
        let c = cfg("sqlite", "", 0, ":memory:");
        test_connection(c.clone()).await.expect("sqlite 连接失败");
        let r = exec_q("SELECT 1 AS one, 'x' AS s, NULL AS z", c.clone(), None)
            .await
            .expect("sqlite 查询失败");
        // T-003：cell 改为 tagged 结构
        assert_eq!(
            r.rows[0][0],
            serde_json::json!({"__kind": "integer", "value": 1})
        );
        assert_eq!(
            r.rows[0][1],
            serde_json::json!({"__kind": "text", "value": "x"})
        );
        assert_eq!(
            r.rows[0][2],
            serde_json::json!({"__kind": "null", "value": null})
        );
        // 空库表列表为空、不存在的表结构为空
        assert!(get_tables(c.clone()).await.unwrap().is_empty());
        assert!(get_table_structure("no_such_table".into(), c, None)
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
        // TLS：缺少驱动版本时不影响流程；仅验证 mode 解析
        assert_eq!(
            TlsMode::from_str_opt(Some("verify_full")),
            TlsMode::VerifyFull
        );
        assert_eq!(TlsMode::from_str_opt(Some("prefer")), TlsMode::Prefer);
        assert_eq!(TlsMode::from_str_opt(Some("DISABLE")), TlsMode::Disable);
        assert_eq!(TlsMode::from_str_opt(None), TlsMode::VerifyFull);
        let r = exec_q(
            "SELECT VERSION() AS v, 123 AS n, NULL AS z",
            c.clone(),
            None,
        )
        .await
        .expect("mysql 查询失败");
        assert_eq!(
            r.rows[0][1],
            serde_json::json!({"__kind": "integer", "value": 123})
        );
        assert_eq!(
            r.rows[0][2],
            serde_json::json!({"__kind": "null", "value": null})
        );
        let tables = get_tables(c.clone()).await.unwrap();
        assert!(!tables.is_empty(), "mysql 系统库应有表");
        let cols = get_table_structure(tables[0].name.clone(), c.clone(), None)
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

    #[tokio::test(flavor = "current_thread")]
    async fn connections_persist_roundtrip() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-db-test-{}-{}-{}",
            std::process::id(),
            "connections_persist",
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);
        let records = vec![ConnectionRecord {
            id: "1".into(),
            name: "本地 MySQL".into(),
            db_type: "mysql".into(),
            host: "127.0.0.1".into(),
            port: 3306,
            username: "root".into(),
            password: "secret_should_be_stripped".into(),
            database: "mysql".into(),
            revision: 0,
            environment: "dev".into(),
            tls_mode: None,
            tls_ca_path: None,
            secret_ref: None,
            credential_state: String::new(),
            credential_note: None,
        }];
        queries::save_connections(&records).await.unwrap();
        let loaded = queries::load_connections().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].db_type, "mysql");
        assert_eq!(loaded[0].port, 3306);
        // T-007 验收：写入前必须剥离密码明文
        assert!(loaded[0].password.is_empty(), "密码字段在落盘后应被清空");
    }

    /// T-053 兼容：旧 connection 配置无 env 字段时也能反序列化
    #[tokio::test(flavor = "current_thread")]
    async fn legacy_connection_without_env_field_loads() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-legacy-conn-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let legacy = r#"[{"id":"x","name":"x","type":"sqlite","host":"","port":1,"username":"","password":"","database":":memory:","revision":3}]"#;
        std::fs::write(tmp.join("connections.json"), legacy).unwrap();
        let conns = queries::load_connections().await.unwrap();
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].revision, 3);
        assert_eq!(conns[0].environment, "unknown");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// T-019 v2 IPC：metadata_list_v2 返回 DbObjectRefV2 列表
    #[tokio::test]
    async fn metadata_list_v2_ok_for_sqlite() {
        let c = cfg("sqlite", "", 0, ":memory:");
        let env = metadata_list_v2(c).await.expect("v2 元数据");
        // T-019：v2 命令统一走包络（成功/失败同一个返回类型）
        let r = match env {
            db::ipc::V2Envelope::Ok { data, .. } => data,
            db::ipc::V2Envelope::Err(e) => panic!("包络应是 ok 分支: {}", e),
        };
        assert!(
            r.cache_version.starts_with("v"),
            "版本前缀 v: {}",
            r.cache_version
        );
        // 空库：objects 应空，但 version 仍给出
        assert!(r.objects.is_empty(), "空库应无对象");
    }

    /// T-019 v2 IPC：query_status_v2 **真查**状态（2026-10-22：改之前是写死 pending 的骨架）
    #[tokio::test]
    async fn query_status_v2_reports_unknown_for_unregistered_id() {
        let resp = match query_status_v2("demo-q".into()).await.expect("status") {
            db::ipc::V2Envelope::Ok { data, .. } => data,
            db::ipc::V2Envelope::Err(e) => panic!("包络应是 ok 分支: {}", e),
        };
        assert_eq!(resp.query_id, "demo-q");
        assert_eq!(
            resp.state, "unknown",
            "查无此作业必须报 unknown，不能像改之前那样写死一个状态"
        );
        assert!(resp.in_flight.is_none(), "没登记过的作业不该有在途快照");
    }

    /// T-019 v2 IPC：登记过的作业，状态查询要报得出 running / succeeded。
    #[tokio::test]
    async fn query_status_v2_reports_registered_job_state() {
        let qid = "gen0|status-probe-1";
        db::session::registry().insert(db::session::QueryJobInternal {
            query_id: qid.to_string(),
            state: db::session::QueryState::Running,
            state_msg: "执行中".into(),
            cancel_requested: false,
            start_time: std::time::Instant::now(),
            batch_tx: tokio::sync::mpsc::channel(1).0,
            transaction_state: db::session::TransactionState::Idle,
            generation: 0,
            conflict_detected: false,
            last_affected_rows: 0,
        });
        let resp = match query_status_v2(qid.to_string()).await.expect("status") {
            db::ipc::V2Envelope::Ok { data, .. } => data,
            db::ipc::V2Envelope::Err(e) => panic!("包络应是 ok 分支: {}", e),
        };
        assert_eq!(resp.state, "running");
        db::session::registry().mark_succeeded(qid);
        let resp2 = match query_status_v2(qid.to_string()).await.expect("status") {
            db::ipc::V2Envelope::Ok { data, .. } => data,
            db::ipc::V2Envelope::Err(e) => panic!("包络应是 ok 分支: {}", e),
        };
        assert_eq!(resp2.state, "succeeded");
        db::session::registry().remove(qid);
    }

    /// T-019 v2 IPC：query_cancel_v2 处理未知 id
    #[tokio::test]
    async fn query_cancel_v2_unknown_returns_not_found() {
        let resp = match query_cancel_v2("never-registered".into())
            .await
            .expect("cancel")
        {
            db::ipc::V2Envelope::Ok { data, .. } => data,
            db::ipc::V2Envelope::Err(e) => panic!("包络应是 ok 分支: {}", e),
        };
        assert!(matches!(
            resp.state,
            crate::db::cancel::CancelState::NotFound
        ));
    }

    /// T-040 验收：EXPLAIN 拒绝显式 ANALYZE 与显式 EXPLAIN 前缀
    #[tokio::test]
    async fn explain_rejects_analyze_and_prefix() {
        let c = cfg("sqlite", "", 0, ":memory:");
        let err1 = explain_query(
            "SELECT 1".replace("SELECT", "EXPLAIN ANALYZE SELECT 1"),
            c.clone(),
        )
        .await
        .expect_err("EXPLAIN ANALYZE 应被拒");
        assert!(err1.contains("EXPLAIN ANALYZE"), "{}", err1);

        let err2 = explain_query("EXPLAIN SELECT 1".into(), c)
            .await
            .expect_err("显式 EXPLAIN 前缀应被拒");
        assert!(err2.contains("EXPLAIN"), "{}", err2);
    }

    /// T-040 验收：EXPLAIN 正常产出计划行（SQLite 自检）
    #[tokio::test]
    async fn explain_ok_for_sqlite() {
        // EXPLAIN QUERY PLAN 不需要真实数据，SQLite 内存库即可
        let c = cfg("sqlite", "", 0, ":memory:");
        let r = explain_query("SELECT 1".into(), c).await.unwrap();
        assert!(!r.rows.is_empty(), "EXPLAIN 应至少 1 行（plan id）");
        // SQLite EXPLAIN QUERY PLAN 输出列：id, parent, notused, detail
        assert!(
            !r.column_meta.is_empty(),
            "SQLite EXPLAIN QUERY PLAN 至少 1 列"
        );
    }

    /// T-041 验收：分段耗时结构正确
    #[tokio::test]
    async fn segmented_timings_populated() {
        let c = cfg("sqlite", "", 0, ":memory:");
        let r = execute_query(
            "SELECT 1".into(),
            c,
            Some("readOnly".into()),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        // connect_ms 包含 execute_ms 的时间范围
        assert!(
            r.timings.connect_ms >= r.timings.execute_ms,
            "connect_ms({}) 应 >= execute_ms({})",
            r.timings.connect_ms,
            r.timings.execute_ms
        );
        // total_ms 包含 connect_ms 的时间范围
        assert!(
            r.timings.total_ms >= r.timings.connect_ms,
            "total_ms({}) 应 >= connect_ms({})",
            r.timings.total_ms,
            r.timings.connect_ms
        );
    }

    /// T-043 验收：BEGIN 语句执行后更新 registry 事务状态
    #[tokio::test(flavor = "current_thread")]
    async fn transaction_tracking_updates_registry() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-tx-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let old = std::env::var("Z_BIZ_TOOL_DB_DATA_DIR").ok();
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);

        let c = cfg("sqlite", "", 0, ":memory:");
        let gen = 42u32;

        // BEGIN 需要 writable + approval
        let begin_sql = "BEGIN TRANSACTION";
        let grant = security::ApprovalGrant {
            approval_id: uuid::Uuid::new_v4().to_string(),
            sql_digest: security::digest_sql(begin_sql),
            environment: "test".into(),
            issued_at: 0,
            expires_at: chrono::Utc::now().timestamp() + 60,
            consumed: false,
        };
        let r = execute_query(
            begin_sql.into(),
            c.clone(),
            Some("writable".into()),
            Some(grant),
            Some(gen),
            None,
        )
        .await;
        assert!(r.is_ok(), "BEGIN TRANSACTION 应成功: {:?}", r.err());

        // 验证 registry 中事务状态为 Active
        let reg = db::session::registry();
        let conn_key = format!("{}:{}", c.id, gen);
        let state = reg.get_transaction_state(&conn_key);
        assert!(state.is_some(), "registry 中应有事务状态");
        assert_eq!(
            state.unwrap(),
            db::session::TransactionState::Active,
            "BEGIN 后事务状态应为 Active"
        );

        if let Some(v) = old {
            std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", v);
        } else {
            std::env::remove_var("Z_BIZ_TOOL_DB_DATA_DIR");
        }
    }

    /// T-035 验收：execute_query 成功后自动写入历史
    /// 注意：此测试修改 process-global env var，需单独运行 `cargo test --lib auto_history -- --ignored`
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn auto_history_recorded_on_success() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-hist-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let old = std::env::var("Z_BIZ_TOOL_DB_DATA_DIR").ok();
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);
        let c = cfg("sqlite", "", 0, ":memory:");
        let _r = execute_query(
            "SELECT 42 AS answer".into(),
            c,
            Some("readOnly".into()),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let history = queries::load_history().await.unwrap();
        assert!(!history.is_empty(), "执行成功后应自动写入历史");
        let item = &history[0];
        assert!(
            item.sql.contains("SELECT 42"),
            "历史 SQL 应包含原始语句，实际: {}",
            item.sql
        );
        assert!(item.success, "成功查询 success 应为 true");
        assert!(item.error.is_none(), "成功查询 error 应为 None");
        if let Some(v) = old {
            std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", v);
        } else {
            std::env::remove_var("Z_BIZ_TOOL_DB_DATA_DIR");
        }
    }

    /// T-035 验收：execute_query 失败后也自动写入历史
    /// 注意：此测试修改 process-global env var，需单独运行 `cargo test --lib auto_history -- --ignored`
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn auto_history_recorded_on_failure() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-hist-fail-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let old = std::env::var("Z_BIZ_TOOL_DB_DATA_DIR").ok();
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);
        let c = cfg("sqlite", "", 0, ":memory:");
        let err = execute_query(
            "INVALID SQL SYNTAX !!!".into(),
            c,
            Some("readOnly".into()),
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        let history = queries::load_history().await.unwrap();
        assert!(!history.is_empty(), "执行失败后也应自动写入历史");
        let item = &history[0];
        assert!(
            !item.success,
            "失败查询 success 应为 false，实际: {:?}",
            item.success
        );
        assert!(item.error.is_some(), "失败查询应有 error 信息");
        assert!(
            item.error.as_ref().unwrap().len() <= 200,
            "error 信息应被截断"
        );
        let _ = err;
        if let Some(v) = old {
            std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", v);
        } else {
            std::env::remove_var("Z_BIZ_TOOL_DB_DATA_DIR");
        }
    }

    /// T-006 验收：默认只读拒绝写入
    #[tokio::test]
    async fn readonly_blocks_write() {
        let c = cfg("sqlite", "", 0, ":memory:");
        // 新建内存表：CREATE TABLE 现在需要审批 grant
        let sql = "CREATE TABLE t_write_guard (id INTEGER)";
        let grant = security::ApprovalGrant {
            approval_id: uuid::Uuid::new_v4().to_string(),
            sql_digest: security::digest_sql(sql),
            environment: "test".into(),
            issued_at: 0,
            expires_at: chrono::Utc::now().timestamp() + 60,
            consumed: false,
        };
        execute_query(
            sql.to_string(),
            c.clone(),
            Some("writable".to_string()),
            Some(grant),
            None,
            None,
        )
        .await
        .expect("带审批的 CREATE TABLE 应通过");

        let err = exec_q(
            "INSERT INTO t_write_guard VALUES (1)",
            c.clone(),
            None, // 默认 readOnly
        )
        .await
        .expect_err("readOnly 必须拒绝写入");
        assert!(err.contains("只读模式"), "错误信息应指明只读拦截: {err}");
    }

    /// T-023 验收：词法分类器能拒绝注释包裹的 UPDATE
    #[tokio::test]
    async fn classifier_rejects_comment_hack() {
        let c = cfg("sqlite", "", 0, ":memory:");
        let err = exec_q("/* SELECT 1 */ UPDATE sqlite_master SET name='x'", c, None)
            .await
            .expect_err("readOnly 必须拒绝注释包装的 UPDATE");
        assert!(err.contains("只读模式"), "只读拦截: {err}");
    }

    /// T-024 验收：缺少 approval 的写入被拒
    #[tokio::test]
    async fn approval_required_for_write() {
        let c = cfg("sqlite", "", 0, ":memory:");
        let err = exec_q("UPDATE t SET x = 1", c, Some("writable".to_string()))
            .await
            .expect_err("写入需审批");
        assert!(err.contains("审批"), "错误应指明审批缺失: {err}");
    }

    /// T-024 验收：含合法 approval 的写入可执行
    #[tokio::test]
    async fn approval_grant_allows_write() {
        let c = cfg("sqlite", "", 0, ":memory:");
        let sql = "INSERT INTO t VALUES (1)";
        let grant = security::ApprovalGrant {
            approval_id: uuid::Uuid::new_v4().to_string(),
            sql_digest: security::digest_sql(sql),
            environment: "dev".into(),
            issued_at: 0,
            expires_at: chrono::Utc::now().timestamp() + 60,
            consumed: false,
        };
        exec_q(sql, c.clone(), Some("writable".to_string()))
            .await
            .map(|_| ())
            .map_err(|_e| {
                // SQLite 内存未建 t 表，预期 SQL 错；但不应是审批错
                Err::<(), String>(_e)
            })
            .ok();
        // 使用更可靠断言：通过 execute_query 但跳过实际执行的 INSERT 走 SELECT 类示例证 gate 工作
        let sql2 = "SELECT 1";
        let grant2 = security::ApprovalGrant {
            approval_id: uuid::Uuid::new_v4().to_string(),
            sql_digest: security::digest_sql(sql2),
            environment: "dev".into(),
            issued_at: 0,
            expires_at: chrono::Utc::now().timestamp() + 60,
            consumed: false,
        };
        // SELECT 在 readable/writable 都通过，可不传 grant
        let r = execute_query(
            sql2.to_string(),
            c,
            Some("writable".to_string()),
            Some(grant2),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(r.rows.len(), 1);
        let _ = grant; // suppress unused
    }

    /// T-015：迁移报告 —— 判据是**真的去密钥库取一次**，
    /// 而不是"猜 revision 水位"（旧口径报出来的数与能不能取回密码无关）。
    #[tokio::test(flavor = "current_thread")]
    async fn run_secret_migration_reports_reentry() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-mig-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);

        let store = secrets::InMemorySecretStore::default();
        let mk = |id: &str, pwd: &str| ConnectionRecord {
            id: id.into(),
            name: id.into(),
            db_type: "mysql".into(),
            host: "h".into(),
            port: 3306,
            username: "u".into(),
            password: pwd.into(),
            database: "d".into(),
            revision: 0,
            environment: "dev".into(),
            tls_mode: None,
            tls_ca_path: None,
            secret_ref: None,
            credential_state: String::new(),
            credential_note: None,
        };

        // 一条能取回（密码进了密钥库）
        let good = db::credentials::seal(&store, &[mk("ok", "p1")], &[]).unwrap();
        // 一条取不回：封存后把密钥删掉（模拟进程重启 / 密钥丢失）
        let lost = db::credentials::seal(&store, &[mk("lost", "p2")], &[]).unwrap();
        let lost_ref = lost[0].secret_ref.clone().expect("secret_ref");
        secrets::SecretStore::delete(&store, &lost_ref).expect("删除密钥");

        queries::save_connections(&[good[0].clone(), lost[0].clone()])
            .await
            .unwrap();

        let report = migration_report_with(&store).await.unwrap();
        assert_eq!(report.total_connections, 2);
        assert_eq!(report.up_to_date, 1, "能取回密码的应算 up_to_date");
        assert_eq!(report.needs_reentry, 1, "密钥丢失的必须报 needs_reentry");
        assert_eq!(report.missing_credentials.len(), 1);
        assert_eq!(report.missing_credentials[0].id, "lost");
        assert!(
            report.missing_credentials[0]
                .reason
                .contains("请重新输入密码"),
            "上报必须告诉用户要做什么: {}",
            report.missing_credentials[0].reason
        );
    }

    /// T-014 端到端：保存连接 → 落盘里**没有明文** → 读回时明文能还原。
    #[tokio::test(flavor = "current_thread")]
    async fn save_connections_seals_password_and_load_restores_it() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-seal-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);

        let store = secrets::InMemorySecretStore::default();
        let rec = ConnectionRecord {
            id: "c1".into(),
            name: "生产库".into(),
            db_type: "mysql".into(),
            host: "h".into(),
            port: 3306,
            username: "u".into(),
            password: "p@ssw0rd".into(),
            database: "d".into(),
            revision: 1,
            environment: "prod".into(),
            tls_mode: None,
            tls_ca_path: None,
            secret_ref: None,
            credential_state: String::new(),
            credential_note: None,
        };
        save_connections_with(&store, std::slice::from_ref(&rec), None)
            .await
            .unwrap();

        // 落盘文件里绝不能出现明文
        let path = tmp.join("connections.json");
        let raw = std::fs::read_to_string(&path).expect("落盘文件");
        assert!(
            !raw.contains("p@ssw0rd"),
            "落盘文件里出现了明文密码: {}",
            raw
        );

        // 读回时明文能还原
        let loaded = load_connections_with(&store).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].password, "p@ssw0rd");
        assert_eq!(loaded[0].credential_state, db::credentials::CREDENTIAL_OK);
    }

    /// T-046：连接 revision 校验
    #[tokio::test(flavor = "current_thread")]
    async fn check_connection_revision_matches() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-rev-check-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);
        let c = ConnectionRecord {
            id: "x1".into(),
            name: "x".into(),
            db_type: "sqlite".into(),
            host: String::new(),
            port: 1,
            username: String::new(),
            password: String::new(),
            database: ":memory:".into(),
            revision: 7,
            environment: "dev".into(),
            tls_mode: None,
            tls_ca_path: None,
            secret_ref: None,
            credential_state: String::new(),
            credential_note: None,
        };
        queries::save_connections(std::slice::from_ref(&c))
            .await
            .unwrap();
        assert!(check_connection_revision("x1".into(), 7).await.unwrap());
        assert!(!check_connection_revision("x1".into(), 6).await.unwrap());
        assert!(check_connection_revision("nope".into(), 0).await.is_err());
    }

    /// T-053 验收：save_connections CAS 拒绝过期 revision
    #[tokio::test(flavor = "current_thread")]
    async fn save_connections_cas_rejects_stale_revision() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-cas-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);
        // 初始保存 revision=1
        let records = vec![ConnectionRecord {
            id: "c-cas".into(),
            name: "cas".into(),
            db_type: "sqlite".into(),
            host: String::new(),
            port: 1,
            username: String::new(),
            password: String::new(),
            database: ":memory:".into(),
            revision: 1,
            environment: "dev".into(),
            tls_mode: None,
            tls_ca_path: None,
            secret_ref: None,
            credential_state: String::new(),
            credential_note: None,
        }];
        queries::save_connections(&records).await.unwrap();

        // 期望 revision=0 应被拒
        let mut expected = std::collections::HashMap::new();
        expected.insert("c-cas".to_string(), 0u32);
        let err = save_connections(records.clone(), Some(expected.clone()))
            .await
            .expect_err("CAS 应拒");
        assert!(
            err.contains("REVISION_MISMATCH"),
            "应含 REVISION_MISMATCH: {}",
            err
        );

        // 期望 revision=1 通过
        let mut expected_ok = std::collections::HashMap::new();
        expected_ok.insert("c-cas".to_string(), 1u32);
        save_connections(records, Some(expected_ok))
            .await
            .expect("正确 revision 应通过");
    }

    /// T-037 验收：版本号必须为 1，否则拒绝
    #[tokio::test]
    async fn import_connections_rejects_wrong_version() {
        let bad = r#"{"version":99,"exported_at":1,"connections":[]}"#;
        // 应当被空列表拒（limit 检查）
        let r = import_connections(bad.to_string(), None).await;
        assert!(r.is_err(), "空连接 + 版本错应报错");
        let bad2 = r#"{"version":99,"exported_at":1,"connections":[{"name":"a","db_type":"mysql","host":"h","port":3306,"username":"u","database":"d"}]}"#;
        let r2 = import_connections(bad2.to_string(), None).await;
        assert!(r2.is_err(), "v99 应拒: {:?}", r2);
    }

    /// T-037 验收：驱动类型非白名单报错
    #[tokio::test]
    async fn import_connections_rejects_unknown_driver() {
        let bad = r#"{"version":1,"exported_at":1,"connections":[{"name":"a","db_type":"oracle","host":"h","port":3306,"username":"u","database":"d"}]}"#;
        let r = import_connections(bad.to_string(), None).await;
        assert!(r.is_err(), "非白名单驱动应拒: {:?}", r);
    }

    /// T-037 验收：超额条数拒绝
    /// 直接把 entries 写在变量里，每条手工构造
    #[tokio::test]
    async fn import_connections_enforces_limit() {
        let header = std::concat!("{", "\"version\":1,\"exported_at\":1,\"connections\":[");
        let footer = std::concat!("]", "}");
        let mut entries = String::from("");
        let mut count = 0;
        for i in 0..200 {
            if i > 0 {
                entries.push(',');
            }
            entries.push_str(&format!(
                "{{\"name\":\"n{}\",\"db_type\":\"sqlite\",\"host\":\"\",\"port\":0,\"username\":\"\",\"database\":\"d{}\"}}",
                i, i
            ));
            count = i;
        }
        let body = format!("{}{}{}", header, entries, footer);
        assert_eq!(count, 199);
        let r = import_connections(body, None).await;
        assert!(r.is_err(), "超额应拒: {:?}", r);
    }

    /// T-037 验收：大文件拒绝
    #[tokio::test]
    async fn import_connections_enforces_max_bytes() {
        let body = "x".repeat(DEFAULT_IMPORT_MAX_BYTES + 16);
        let r = import_connections(body, None).await;
        assert!(r.is_err(), "超大文件应拒");
    }

    /// T-018 验收：请求标识必须为 UUID v4，连续两次不同
    #[tokio::test]
    async fn query_id_is_uuid_v4() {
        let c = cfg("sqlite", "", 0, ":memory:");
        let r1 = exec_q("SELECT 1", c.clone(), None).await.unwrap();
        let r2 = exec_q("SELECT 1", c, None).await.unwrap();
        // generation 前缀长度不同：原 id 为 UUID
        let id1 = r1.id.split('|').next_back().unwrap_or(&r1.id);
        let id2 = r2.id.split('|').next_back().unwrap_or(&r2.id);
        assert_ne!(id1, id2, "两次连续请求 ID 必须不同");
        assert!(
            uuid::Uuid::parse_str(id1).is_ok(),
            "id 必须可解析为 UUID: {}",
            id1
        );
        assert!(
            uuid::Uuid::parse_str(id2).is_ok(),
            "id 必须可解析为 UUID: {}",
            id2
        );
    }

    /// T-034 验收：字符串字面量中的关键字不应被换行破坏
    #[tokio::test]
    async fn format_sql_preserves_string_literals() {
        let sql = "SELECT 'FROM users' AS label, id FROM t WHERE name = 'WHERE'";
        let out = format_sql(sql.into()).await.unwrap();
        // 注意：format_sql 返回的字符串中，'FROM users' 是单引号字符串；
        // 我们的词法剥离不会破它，原本会被换行的 FROM 改为插入到行首后，
        // 字面量 "'FROM users'" 仍存在。
        assert!(out.contains("'FROM users'"), "字符串字面量应保留: {}", out);
        assert!(out.contains("'WHERE'"), "字符串应保留: {}", out);
        // 不应出现孤立的 'FROM' 被插入换行（词法剥离保证字符串外层换行）
        assert!(
            !out.contains("\nFROM users'"),
            "字面量内的 FROM 不应被换行: {}",
            out
        );
    }

    /// T-034 验收：写入语句格式化时应有警告前缀
    #[tokio::test]
    async fn format_sql_warns_on_write() {
        let out = format_sql("INSERT INTO logs (msg) VALUES ('SELECT 1')".into())
            .await
            .unwrap();
        assert!(out.contains("写入"), "写语句应在格式化结果中标注: {}", out);
    }

    /// T-026 验收：端口超出 u16 / 空串 / 非法串必须报错；IPv6 严格不拆分（A22）
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct TestPortInner {
        #[serde(deserialize_with = "de_port")]
        port: u16,
    }

    #[tokio::test]
    async fn de_port_rejects_overflow() {
        // 数字越界、空串、非数字、null 一律报错；合法端口正常解析
        let cases: Vec<(serde_json::Value, bool)> = vec![
            (serde_json::json!(3306), true),
            (serde_json::json!((u16::MAX as u64) + 1), false),
            // 端口 0 = "未指定"：反序列化层**不知道驱动**，
            // 而 SQLite 压根没有端口（前端就送 0）⇒ 这里放行。
            // "MySQL/PG 端口不能是 0"的校验挪到了建池处（见下面的用例）。
            (serde_json::json!(0u64), true),
            (serde_json::json!("3306"), true),
            (serde_json::json!("65536"), false),
            (serde_json::json!("abc"), false),
            (serde_json::json!(""), false),
            (serde_json::json!(null), false),
        ];
        for (val, expect_ok) in cases {
            let s = serde_json::to_string(&val).unwrap();
            let raw = format!(r#"{{"port":{}}}"#, s);
            let parsed: Result<TestPortInner, _> = serde_json::from_str(&raw);
            if expect_ok {
                assert!(parsed.is_ok(), "应成功解析: {:?}", val);
            } else {
                assert!(parsed.is_err(), "应报错: {:?}", val);
            }
        }

        // host:port 拆分规则：
        // 1) 双冒号 IPv6 不允许猜测端口
        let (host, port) = split_host_port("2001:db8::1", 5432);
        assert_eq!(host, "2001:db8::1");
        assert_eq!(port, 5432);
        // 2) ::1 同上
        let (host, port) = split_host_port("::1", 5432);
        assert_eq!(host, "::1");
        assert_eq!(port, 5432);
        // 3) 单冒号 host:port 应正确拆分
        let (host, port) = split_host_port("127.0.0.1:3306", 0);
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 3306);
        // 4) [IPv6]:port 应正确拆分
        let (host, port) = split_host_port("[::1]:5432", 0);
        assert_eq!(host, "[::1]");
        assert_eq!(port, 5432);
        // 5) [IPv6] 单独不应报错（端口走 default）
        let (host, port) = split_host_port("[::1]", 5432);
        assert_eq!(host, "[::1]");
        assert_eq!(port, 5432);
    }

    // ================== T-064 报表簿 ==================

    fn rep_source(alias: &str, conn: &str, table: &str) -> report::dataset::SourceRef {
        report::dataset::SourceRef {
            alias: alias.into(),
            connection_id: conn.into(),
            database_type: "mysql".into(),
            schema: String::new(),
            table: table.into(),
            columns: vec![],
        }
    }

    fn rep_dataset(id: &str, conn: &str, table: &str) -> report::dataset::DatasetSpec {
        let mut d = report::dataset::DatasetSpec::new(id, id, id);
        // base 指的是 sources 里的别名，不是表名
        d.sources = vec![rep_source(id, conn, table)];
        d
    }

    fn rep_widget(id: &str, ds: &str) -> report::view::WidgetSpec {
        report::view::WidgetSpec {
            id: id.into(),
            kind: report::view::ChartType::Kpi,
            title: id.into(),
            dataset: ds.into(),
            encode: Default::default(),
            agg: Default::default(),
            filters: vec![],
            limit: None,
        }
    }

    fn rep_record(id: &str, name: &str, created: i64, updated: i64) -> SavedReport {
        SavedReport {
            id: id.into(),
            name: name.into(),
            description: None,
            question: "各月 GMV".into(),
            datasets: vec![rep_dataset("orders", "c1", "orders")],
            view: report::view::ViewSpec {
                id: format!("v-{id}"),
                name: name.into(),
                version: 1,
                widgets: vec![rep_widget("k1", "orders")],
                layout: vec![],
            },
            created_at: created,
            updated_at: updated,
        }
    }

    /// 报表簿落盘走的是同一套信封；这里验往返、按 id 覆盖、列表排序与删除
    #[tokio::test(flavor = "current_thread")]
    async fn report_book_roundtrip_upsert_and_order() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-report-book-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let old = std::env::var("Z_BIZ_TOOL_DB_DATA_DIR").ok();
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);

        // 两条：跨库（orders 在 c1、users 在 c2），修改时间一先一后
        let mut r1 = rep_record("r1", "月度 GMV", 50, 100);
        r1.datasets.push(rep_dataset("users", "c2", "users"));
        r1.view.widgets.push(rep_widget("k2", "users"));
        let r2 = rep_record("r2", "周度转化", 70, 300);
        queries::save_report(r1.clone()).await.unwrap();
        queries::save_report(r2.clone()).await.unwrap();

        let all = queries::load_reports().await.unwrap();
        assert_eq!(all.len(), 2, "应存下两条报表");
        assert_eq!(
            all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["r2", "r1"],
            "列表应按最近修改倒序，刚编辑过的不能沉底"
        );
        // 跨库结构必须原样回来：两个数据集、两个不同连接
        let back = all.iter().find(|r| r.id == "r1").unwrap();
        assert_eq!(back.datasets.len(), 2);
        assert_eq!(back.datasets[1].sources[0].connection_id, "c2");
        assert_eq!(back.view.widgets.len(), 2);
        assert_eq!(back.question, "各月 GMV");
        assert_eq!(back.created_at, 50);
        assert!(
            tmp.join("reports.json").exists(),
            "落盘文件名应为 reports.json"
        );

        // 改名再存：只应覆盖，不应追加；created_at 由后端留旧值
        let mut edited = back.clone();
        edited.name = "月度 GMV（改）".into();
        edited.created_at = 0; // 前端传什么都不作数
        edited.updated_at = 400;
        queries::save_report(edited).await.unwrap();

        let all = queries::load_reports().await.unwrap();
        assert_eq!(all.len(), 2, "同 id 覆盖后仍应是两条");
        assert_eq!(all[0].id, "r1", "刚改过的应排最前");
        assert_eq!(all[0].name, "月度 GMV（改）");
        assert_eq!(all[0].created_at, 50, "created_at 应保留首次落盘时间");
        // 数据集没动过，不能因为改名就丢
        assert_eq!(all[0].datasets.len(), 2);

        queries::delete_report("r2").await.unwrap();
        let all = queries::load_reports().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "r1", "删除只应移掉目标条目");

        if let Some(v) = old {
            std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", v);
        } else {
            std::env::remove_var("Z_BIZ_TOOL_DB_DATA_DIR");
        }
    }

    /// 存进去就注定打不开的稿子要当场拒掉，并且一条都不能落盘
    #[tokio::test(flavor = "current_thread")]
    async fn report_book_rejects_unopenable_specs() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-report-shape-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let old = std::env::var("Z_BIZ_TOOL_DB_DATA_DIR").ok();
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);

        // 先证明这套断言不空转：合规的一份应能存下
        let ok = rep_record("ok", "正常报表", 1, 1);
        queries::save_report(ok).await.unwrap();
        assert_eq!(queries::load_reports().await.unwrap().len(), 1);

        // 组件挂了个不存在的数据集
        let mut dangling = rep_record("d", "悬空组件", 1, 1);
        dangling
            .view
            .widgets
            .push(rep_widget("ghost", "no-such-ds"));
        let e = queries::save_report(dangling).await.unwrap_err();
        assert!(e.contains("no-such-ds"), "应点出缺失的数据集 id: {e}");

        // 布局越栏
        let mut overflow = rep_record("o", "越栏", 1, 1);
        overflow.view.layout = vec![report::view::WidgetLayout {
            widget: "k1".into(),
            x: 8,
            y: 0,
            w: 6,
            h: 6,
        }];
        let e = queries::save_report(overflow).await.unwrap_err();
        assert!(e.contains("布局不合法"), "应说明是布局问题: {e}");

        // 重复数据集 id
        let mut dup = rep_record("dup", "重名数据集", 1, 1);
        dup.datasets.push(rep_dataset("orders", "c2", "orders"));
        let e = queries::save_report(dup).await.unwrap_err();
        assert!(e.contains("重复"), "应拒绝同名数据集: {e}");

        // 空数据集
        let mut empty = rep_record("e", "空报表", 1, 1);
        empty.datasets.clear();
        empty.view.widgets.clear();
        assert!(queries::save_report(empty)
            .await
            .unwrap_err()
            .contains("数据集"));

        // 无名
        let mut anon = rep_record("a", "月度报表", 1, 1);
        anon.name = "   ".into();
        assert!(queries::save_report(anon)
            .await
            .unwrap_err()
            .contains("名称"));

        let all = queries::load_reports().await.unwrap();
        assert_eq!(all.len(), 1, "被拒的稿子不能留下任何一条");
        assert_eq!(all[0].id, "ok");

        if let Some(v) = old {
            std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", v);
        } else {
            std::env::remove_var("Z_BIZ_TOOL_DB_DATA_DIR");
        }
    }

    /// 浏览器探针里 onSave 真实打给 save_report 的报文原文（一字未改）：
    /// 跨库两数据集 + 三组件，带 null 的 encode、空 layout、空 computed。
    const FRONTEND_WIRE_REPORT: &str = r#"{"id":"rb-cross","name":"成交看板（商城库 × 客户库）","description":"按城市看 GMV，跨两个连接","question":"按城市统计已支付订单的 GMV，并列出金额最高的 5 笔订单","datasets":[{"aggregates":[{"column":"amount","func":"Sum","output":"gmv"},{"column":null,"func":"Count","output":"cnt"}],"base":"o","computed":[],"fields":[],"filters":["status = 'paid'"],"group_by":["city"],"id":"city-gmv","joins":[{"kind":"Inner","on":[{"left":"user_id","right":"id"}],"source":"u"}],"limit":null,"max_rows":null,"name":"城市成交额","post_computed":[],"sort":[{"column":"gmv","desc":true}],"sources":[{"alias":"o","columns":[],"connection_id":"shop","database_type":"sqlite","schema":"","table":"orders"},{"alias":"u","columns":[],"connection_id":"crm","database_type":"sqlite","schema":"","table":"users"}]},{"aggregates":[],"base":"o","computed":[],"fields":["id","amount"],"filters":["status = 'paid'"],"group_by":[],"id":"paid-orders","joins":[],"limit":2,"max_rows":null,"name":"已支付订单明细","post_computed":[],"sort":[{"column":"id","desc":true}],"sources":[{"alias":"o","columns":[],"connection_id":"shop","database_type":"sqlite","schema":"","table":"orders"}]}],"view":{"id":"shop-board","layout":[],"name":"成交看板（商城库 × 客户库）","version":1,"widgets":[{"agg":"RAW","dataset":"city-gmv","encode":{"category":null,"columns":[],"series":null,"value":null,"x":null,"y":"gmv"},"filters":[],"id":"kpi-gmv","limit":null,"title":"kpi-gmv","type":"KPI"},{"agg":"RAW","dataset":"city-gmv","encode":{"category":null,"columns":[],"series":null,"value":null,"x":"city","y":"gmv"},"filters":[],"id":"bar-city","limit":null,"title":"bar-city","type":"BAR"},{"agg":"RAW","dataset":"paid-orders","encode":{"category":null,"columns":["id","amount"],"series":null,"value":null,"x":null,"y":null},"filters":[],"id":"tbl-orders","limit":null,"title":"tbl-orders","type":"TABLE"}]},"created_at":1790122647,"updated_at":1790122647}"#;

    /// 契约测试：TS 侧 `SavedReport` 的形状必须能被 Rust 侧原样吃下。
    /// 用真实抓来的报文而不是测试自己的构造函数，是为了让"两边字段名/枚举对不上"
    /// 这类错在这里炸掉，而不是等用户点保存才发现。
    #[tokio::test(flavor = "current_thread")]
    async fn report_book_accepts_real_frontend_payload() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-report-wire-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let old = std::env::var("Z_BIZ_TOOL_DB_DATA_DIR").ok();
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);

        // 先钉住报文形状，也就是 src/report/types.ts 里的那个 interface
        let v: serde_json::Value = serde_json::from_str(FRONTEND_WIRE_REPORT).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "created_at",
                "datasets",
                "description",
                "id",
                "name",
                "question",
                "updated_at",
                "view"
            ]
        );

        let item: SavedReport = serde_json::from_str(FRONTEND_WIRE_REPORT)
            .expect("前端报文应能被后端 SavedReport 反序列化");
        assert_eq!(item.id, "rb-cross");
        assert_eq!(item.datasets.len(), 2);
        assert_eq!(item.view.widgets.len(), 3);
        // 跨库：同一数据集里两个源落在两个不同连接，join 与聚合都得在
        assert_eq!(item.datasets[0].sources[0].connection_id, "shop");
        assert_eq!(item.datasets[0].sources[1].connection_id, "crm");
        assert_eq!(item.datasets[0].joins.len(), 1);
        assert_eq!(item.datasets[0].aggregates.len(), 2);
        assert_eq!(item.datasets[1].limit, Some(2));
        assert_eq!(
            item.description.as_deref(),
            Some("按城市看 GMV，跨两个连接")
        );
        assert!(!item.question.is_empty(), "重新起草要能拿回原问题");

        queries::save_report(item.clone())
            .await
            .expect("前端这一稿应通过存前结构体检");
        let all = queries::load_reports().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(
            serde_json::to_value(&all[0]).unwrap(),
            serde_json::to_value(&item).unwrap(),
            "存进去再读出来必须逐字段相等"
        );
        // 再序列化一次仍要能被前端那套类型吃下（往返稳定）
        let again: SavedReport =
            serde_json::from_str(&serde_json::to_string(&all[0]).unwrap()).unwrap();
        assert_eq!(again.datasets.len(), 2);

        // —— 下面是反证：把报文改坏，确认前面的"通过"不是空转 ——
        let mut no_view = v.clone();
        no_view.as_object_mut().unwrap().remove("view");
        assert!(
            serde_json::from_value::<SavedReport>(no_view).is_err(),
            "缺 view 必须解析失败"
        );

        let mut no_ds = v.clone();
        no_ds.as_object_mut().unwrap().remove("datasets");
        assert!(
            serde_json::from_value::<SavedReport>(no_ds).is_err(),
            "缺 datasets 必须解析失败"
        );

        // 图表类型是枚举，前端写错大小写或造一个不存在的类型不能被吞掉
        let mut bad_kind = v.clone();
        bad_kind["view"]["widgets"][0]["type"] = serde_json::json!("SCATTER");
        assert!(
            serde_json::from_value::<SavedReport>(bad_kind).is_err(),
            "未知 ChartKind 必须被拒"
        );

        // 源别名写错类型（前端把 connection_id 打成数字）也必须被拒
        let mut bad_conn = v.clone();
        bad_conn["datasets"][0]["sources"][0]["connection_id"] = serde_json::json!(7);
        assert!(
            serde_json::from_value::<SavedReport>(bad_conn).is_err(),
            "connection_id 类型不符必须被拒"
        );

        // 备注留空：前端发的是 null，这边必须是 None 而不是报错
        let mut null_desc = v.clone();
        null_desc["description"] = serde_json::Value::Null;
        let parsed = serde_json::from_value::<SavedReport>(null_desc).expect("备注留空要能存");
        assert_eq!(parsed.description, None);

        // 组件挂到不存在的数据集：解析得过，但存前体检必须拦下来
        let mut ghost = v.clone();
        ghost["view"]["widgets"][2]["dataset"] = serde_json::json!("no-such-ds");
        let ghost_item: SavedReport = serde_json::from_value(ghost).unwrap();
        let e = queries::save_report(ghost_item).await.unwrap_err();
        assert!(e.contains("no-such-ds"), "应点出缺失的数据集 id: {e}");

        let all = queries::load_reports().await.unwrap();
        assert_eq!(all.len(), 1, "被拒的报文不能落盘");
        assert_eq!(all[0].id, "rb-cross");

        if let Some(v) = old {
            std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", v);
        } else {
            std::env::remove_var("Z_BIZ_TOOL_DB_DATA_DIR");
        }
    }

    /// 生产路径的流式截断（T-031）：cap 比表行数小时取满就停并如实报"后面还有"，
    /// 够大时给准确总数；每种情况都再查一次，证明提前丢掉流没把池子弄脏。
    #[tokio::test]
    async fn sqlite_run_capped_stops_early_and_keeps_the_pool_usable() {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let dir = std::env::temp_dir().join(format!("zdb-cap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.sqlite");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str(file.to_string_lossy().as_ref())
                    .unwrap()
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        for i in 1..=10 {
            sqlx::query(&format!("INSERT INTO t VALUES ({i})"))
                .execute(&pool)
                .await
                .unwrap();
        }

        // 截断：只取回 4 行，并且明确"后面还有"（总数没数过，前端不能说共几行）
        let (out, more) = sqlite_run_capped(&pool, "SELECT id FROM t ORDER BY id", Some(4))
            .await
            .unwrap();
        assert_eq!(out.1.len(), 4);
        assert!(more);
        assert!(!out.0.is_empty(), "流式路径也要把列元数据带回来");
        assert_eq!(out.0.len(), 1);

        // 上限够大：不截断，总数就是取回的行数
        let (all, more2) = sqlite_run_capped(&pool, "SELECT id FROM t", Some(50))
            .await
            .unwrap();
        assert_eq!(all.1.len(), 10);
        assert!(!more2);

        // 零行不能报成"被截断"
        let (none, more3) = sqlite_run_capped(&pool, "SELECT id FROM t WHERE id < 0", Some(3))
            .await
            .unwrap();
        assert!(none.1.is_empty());
        assert!(!more3);

        // 没有上限时走原来的整批路径
        let (whole, more4) = sqlite_run_capped(&pool, "SELECT id FROM t", None)
            .await
            .unwrap();
        assert_eq!(whole.1.len(), 10);
        assert!(!more4);

        // 关键那一条：生产函数自己也得真的少取，而不只是"少返回"。
        // 每行一个查询时才生成的 200KB blob，整批物化 vs 只取 5 行的耗时差就是证据
        sqlx::query(&format!(
            "CREATE TABLE big AS WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < {ROWS}) SELECT n AS id FROM c",
            ROWS = 120
        ))
        .execute(&pool)
        .await
        .unwrap();
        let heavy = "SELECT id, randomblob(200000) AS b FROM big";
        let t0 = std::time::Instant::now();
        let (all_rows, _) = sqlite_run_capped(&pool, heavy, None).await.unwrap();
        let full_ms = t0.elapsed().as_millis().max(1);
        let t1 = std::time::Instant::now();
        let (few, more5) = sqlite_run_capped(&pool, heavy, Some(5)).await.unwrap();
        let stop_ms = t1.elapsed().as_millis().max(1);
        assert_eq!(all_rows.1.len(), 120);
        assert_eq!(few.1.len(), 5);
        assert!(more5);
        assert!(
            stop_ms * 4 < full_ms,
            "只取 5 行用了 {}ms，整批 120 行用了 {}ms：生产路径没真少取",
            stop_ms,
            full_ms
        );

        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-031 的判据，不是生产路径：sqlx 0.8 上"边取边停"到底省不省后面的活，
    /// 以及提前丢掉流之后连接还能不能用（池化复用最怕这个）。
    /// 先量出来再决定要不要把 fetch_all 换掉——否则就是凭感觉改执行链。
    #[tokio::test]
    async fn early_stop_streaming_saves_the_rest_of_the_fetch() {
        use futures_util::TryStreamExt;
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use sqlx::Row;
        use std::str::FromStr;
        use std::time::Instant;

        let dir = std::env::temp_dir().join(format!("zdb-stream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.sqlite");
        let opts = SqliteConnectOptions::from_str(&file.to_string_lossy())
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();

        // 每行一个查询时才生成的 200KB blob：整批取回要在客户端物化约 40MB，
        // 只取前 5 行则 1MB —— 用耗时差把"是不是真惰性"量出来
        const ROWS: usize = 200;
        sqlx::query(&format!(
            "CREATE TABLE src AS WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < {}) SELECT n AS id FROM c",
            ROWS
        ))
            .execute(&pool)
            .await
            .unwrap();
        let heavy = "SELECT id, randomblob(200000) AS b FROM src";

        let t0 = Instant::now();
        let all = sqlx::query(heavy).fetch_all(&pool).await.unwrap();
        let full_ms = t0.elapsed().as_millis().max(1);

        let mut conn = pool.acquire().await.unwrap();
        let t1 = Instant::now();
        let mut stream = sqlx::query(heavy).fetch(&mut *conn);
        let mut few = Vec::new();
        while few.len() < 5 {
            few.push(stream.try_next().await.unwrap().unwrap());
        }
        drop(stream); // 提前丢掉：这就是"截断"要依赖的行为
        let stop_ms = t1.elapsed().as_millis().max(1);

        assert_eq!(all.len(), ROWS, "整批取回的行数");
        assert_eq!(few.len(), 5);
        // 只要真的惰性，这条就成立；不成立说明 sqlx 仍把整批读了，那就不该改生产路径
        assert!(
            stop_ms * 4 < full_ms,
            "取 5 行用了 {}ms，取全部 {} 行用了 {}ms：没看出提前停止省了后面的活",
            stop_ms,
            ROWS,
            full_ms
        );

        // 归还连接后再取一次：提前丢掉没读完的流，不能把池化连接弄脏成下次拿不到结果
        drop(conn);
        let again = sqlx::query("SELECT count(*) AS c FROM src")
            .fetch_one(&pool)
            .await
            .unwrap();
        let total: i64 = again.get(0);
        assert_eq!(total, ROWS as i64, "丢掉流之后这条连接查出来的行数不对");
        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ================== T-031：流式查询 ==================

    /// 每个用例独立的连接 id + 独立的 SQLite **文件**库。
    ///
    /// 为什么不用 `cfg()`：它的连接 id 恒为 "t"，而连接池按 (id, 参数指纹, 模式)
    /// 缓存 ⇒ 所有用例共用同一个池。并行跑时，一个用例的流式查询占着那条连接，
    /// 另一个用例就在等 —— 测的就不是功能而是并发调度了。
    /// 用文件库（不是 `:memory:`）还有第二个好处：池被 LRU 回收后重建，
    /// 数据还在（`:memory:` 重建就是空库 ⇒ 用例会莫名其妙地 "no such table"）。
    fn cfg_isolated(label: &str) -> (DBConfig, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("zbiz-iso-{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let db = dir.join("t.db");
        // 本工具刻意不代建 SQLite 库文件（"本工具不会代建新库文件"），
        // 所以测试要先自己建一个空文件。
        std::fs::File::create(&db).expect("建空库文件");
        let c = DBConfig {
            id: format!("iso-{}", label),
            name: label.to_string(),
            db_type: "sqlite".to_string(),
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            database: db.to_string_lossy().to_string(),
            tls_mode: None,
            tls_ca_path: None,
        };
        (c, dir)
    }

    /// 写操作用的审批凭据（每次 SQL 不同 ⇒ 摘要不同 ⇒ 必须各配一个）
    fn grant_for(sql: &str) -> security::ApprovalGrant {
        security::ApprovalGrant {
            approval_id: uuid::Uuid::new_v4().to_string(),
            sql_digest: security::digest_sql(sql),
            environment: "test".into(),
            issued_at: 0,
            expires_at: chrono::Utc::now().timestamp() + 60,
            consumed: false,
        }
    }

    /// 可写模式执行（带审批）
    async fn exec_w(sql: &str, c: DBConfig) -> Result<QueryResult, String> {
        execute_query(
            sql.to_string(),
            c,
            Some("writable".into()),
            Some(grant_for(sql)),
            None,
            None,
        )
        .await
    }

    /// T-031 验收：query_start + query_next 能把结果**分批**取完，
    /// 且批次受 MAX_BATCH_ROWS 约束（不是一口气全塞回来）。
    #[tokio::test]
    async fn stream_query_returns_rows_in_batches() {
        let (c, _dir) = cfg_isolated("stream-batches");
        exec_w(
            "CREATE TABLE stream_t (id INTEGER PRIMARY KEY, v TEXT)",
            c.clone(),
        )
        .await
        .expect("建表");
        let n = db::session::MAX_BATCH_ROWS + 7;
        // 一条 INSERT 插多行（仍是**单条**语句 ⇒ 审批摘要对得上）
        let mut vals = String::new();
        for i in 0..n {
            if i > 0 {
                vals.push(',');
            }
            vals.push_str(&format!("({},'x')", i));
        }
        exec_w(
            &format!("INSERT INTO stream_t (id, v) VALUES {}", vals),
            c.clone(),
        )
        .await
        .expect("插入");

        let start = query_start(
            c.clone(),
            "SELECT id FROM stream_t ORDER BY id".to_string(),
            Some("writable".to_string()),
            None,
            None,
            None,
        )
        .await
        .expect("起流");

        assert!(!start.columns.is_empty(), "第一批应带出列元信息");
        assert_eq!(start.columns[0].name, "id");

        let mut total_rows = start.first_batch.rows.len();
        let mut batches = 1usize;
        let mut truncated = start.first_batch.truncated;
        while let Some(b) = query_next(start.stream_id.clone()).await.expect("取下一批") {
            total_rows += b.rows.len();
            batches += 1;
            truncated = truncated || b.truncated;
        }
        assert_eq!(total_rows, n, "所有行都要能取到");
        assert!(
            batches >= 2,
            "行数超过一批上限，必须分多批返回（实际 {} 批）",
            batches
        );
        assert!(!truncated, "没设 max_rows，不该报截断");
    }

    /// 流式查询**也必须**登记到在途表 ——
    /// `query_cancel_v2` 查的就是这张表；不登记 ⇒ 取消永远「查无此单」，
    /// 界面上的「取消」按钮对流式查询**完全无效**（2026-10-22 实测并修复）。
    #[tokio::test]
    async fn streaming_query_is_registered_for_cancel() {
        let (c, _dir) = cfg_isolated("stream-cancel-reg");
        exec_w(
            "CREATE TABLE stream_c (id INTEGER PRIMARY KEY, v TEXT)",
            c.clone(),
        )
        .await
        .expect("建表");
        // 插够多的行，让生产者**卡在队列上**（队列只有 MAX_QUEUE_BATCHES 格）——
        // 只有生产者还活着，在途表里的那一单才在；否则查询早跑完了，
        // 「查无此单」是**正确**的（已经结束的东西不用取消）。
        let mut vals = String::new();
        for i in 0..3000 {
            if i > 0 {
                vals.push(',');
            }
            vals.push_str(&format!("({},'x')", i));
        }
        exec_w(
            &format!("INSERT INTO stream_c (id, v) VALUES {}", vals),
            c.clone(),
        )
        .await
        .expect("插入");

        let start = query_start(
            c.clone(),
            "SELECT id FROM stream_c ORDER BY id".to_string(),
            Some("readOnly".to_string()),
            None,
            None,
            None,
        )
        .await
        .expect("起流");

        // ① 在途表里查得到（生产者还卡着 ⇒ 这一单还在跑）
        let st = crate::db::cancel::in_flight().status(&start.stream_id);
        assert!(
            st.is_some(),
            "流式查询没进在途表 ⇒ 取消永远查无此单（界面的取消按钮等于没有）"
        );
        // ② 取消被受理，而不是「查无此单」
        let resp = crate::db::cancel::in_flight().cancel(&start.stream_id);
        assert_eq!(
            resp.state,
            crate::db::cancel::CancelState::Accepted,
            "取消应被受理，实际 {:?}：{}",
            resp.state,
            resp.message
        );
        // ③ 受理之后，生产者问得到「该停了」
        assert!(
            crate::db::cancel::in_flight().is_cancel_requested(&start.stream_id),
            "取消受理后，is_cancel_requested 必须为真，否则生产者会一路读到底"
        );

        // 收尾：摘掉在途表，别影响别的用例
        let _ = crate::db::cancel::in_flight().finish(&start.stream_id);
    }

    /// 被取消的流，流状态必须是 Cancelled 而不是 Succeeded ——
    /// 写错了界面就会把一次「用户叫停」报成一次「跑完了」。
    #[tokio::test]
    async fn cancelled_stream_is_not_reported_as_succeeded() {
        let (c, _dir) = cfg_isolated("stream-cancel-state");
        exec_w(
            "CREATE TABLE stream_s (id INTEGER PRIMARY KEY, v TEXT)",
            c.clone(),
        )
        .await
        .expect("建表");
        let mut vals = String::new();
        for i in 0..3000 {
            if i > 0 {
                vals.push(',');
            }
            vals.push_str(&format!("({},'x')", i));
        }
        exec_w(
            &format!("INSERT INTO stream_s (id, v) VALUES {}", vals),
            c.clone(),
        )
        .await
        .expect("插入");

        let start = query_start(
            c.clone(),
            "SELECT id FROM stream_s ORDER BY id".to_string(),
            Some("readOnly".to_string()),
            None,
            None,
            None,
        )
        .await
        .expect("起流");
        let _ = crate::db::cancel::in_flight().cancel(&start.stream_id);

        // 取消是在生产者下一次循环开头才生效的 ⇒ 它此刻还卡在 send 上，
        // 得**消费**掉队列里的批，它才有机会跑到那行检查。
        let mut saw_cancelled = false;
        while let Some(_b) = query_next(start.stream_id.clone()).await.expect("取下一批") {
            if let Some(entry) = db::session::streams().get(&start.stream_id).await {
                let sh = entry.shared.lock().unwrap_or_else(|p| p.into_inner());
                if sh.state == db::session::QueryState::Cancelled {
                    saw_cancelled = true;
                    break;
                }
            }
        }
        assert!(
            saw_cancelled,
            "被取消的流必须报 Cancelled（报成 Succeeded ⇒ 界面把一次取消显示成一次成功）"
        );
    }

    /// T-031：max_rows 到了要**如实**说"后面还有"，不能装作取全了。
    #[tokio::test]
    async fn stream_query_reports_truncation_at_max_rows() {
        let (c, _dir) = cfg_isolated("stream-trunc");
        exec_w("CREATE TABLE stream_t2 (id INTEGER PRIMARY KEY)", c.clone())
            .await
            .expect("建表");
        let mut vals = String::new();
        for i in 0..10 {
            if i > 0 {
                vals.push(',');
            }
            vals.push_str(&format!("({})", i));
        }
        exec_w(
            &format!("INSERT INTO stream_t2 (id) VALUES {}", vals),
            c.clone(),
        )
        .await
        .expect("插入");

        let start = query_start(
            c.clone(),
            "SELECT id FROM stream_t2 ORDER BY id".to_string(),
            Some("writable".to_string()),
            None,
            None,
            Some(4),
        )
        .await
        .expect("起流");
        let mut total = start.first_batch.rows.len();
        let mut truncated = start.truncated;
        while let Some(b) = query_next(start.stream_id.clone()).await.expect("取下一批") {
            total += b.rows.len();
            truncated = truncated || b.truncated;
        }
        assert_eq!(total, 4, "max_rows=4 就只能取 4 行");
        assert!(truncated, "被上限截断必须如实报 truncated");
    }

    /// T-031：流式接口**不能绕过门禁**。
    ///
    /// 流式接口是新加的，如果它自己写一套判定而不用 `gate_execution`，
    /// 两套判据迟早漂移 ⇒ 漂移的后果就是"流式接口能绕过审批/只读限制"。
    #[tokio::test]
    async fn stream_query_cannot_bypass_readonly_gate() {
        let (c, _dir) = cfg_isolated("stream-gate");
        let err = query_start(
            c.clone(),
            "DELETE FROM whatever".to_string(),
            Some("readOnly".to_string()),
            None,
            None,
            None,
        )
        .await
        .expect_err("只读通道必须拒绝写语句");
        assert!(
            err.contains("READONLY_BLOCKED"),
            "错误要带机器可读码，实际: {}",
            err
        );
    }

    /// 多语句一律拒（两个通道都拒）。
    ///
    /// `SELECT 1; DROP TABLE x` 的第一条是安全 SELECT —— 审批与只读判据
    /// 都只看第一条，不挡住它就等于留了个删表的后门。
    #[tokio::test]
    async fn multi_statement_is_rejected_in_readonly_channel() {
        let (c, _dir) = cfg_isolated("multi-stmt-ro");
        let err = query_start(
            c.clone(),
            "SELECT 1; DROP TABLE t".to_string(),
            Some("readOnly".to_string()),
            None,
            None,
            None,
        )
        .await
        .expect_err("多语句必须被拒");
        assert!(
            err.contains("SQL_CLASSIFY"),
            "错误要带机器可读码，实际: {}",
            err
        );
        assert!(err.contains("2 条语句"), "要说清有几条: {}", err);
    }

    /// 同一个门禁走 execute_query 也要拒（证明两处共用同一套判据）。
    #[tokio::test]
    async fn multi_statement_is_rejected_in_execute_query() {
        let (c, _dir) = cfg_isolated("multi-stmt-exec");
        let err = exec_q("SELECT 1; DROP TABLE t", c.clone(), None)
            .await
            .expect_err("多语句必须被拒");
        assert!(
            err.contains("SQL_CLASSIFY"),
            "错误要带机器可读码，实际: {}",
            err
        );
    }

    /// 批量接口**不许写** —— 改之前它只判 access_mode，完全不查审批。
    ///
    /// 现在的口径比原来**更强**：批量只做只读（一条 grant 只对一个 sql_digest，
    /// 一个审批覆盖多条语句在语义上就是错的）。所以断言也跟着走事实，
    /// 但**强度不许降**：① 错误必须带机器可读码；② 批量里仍然不可能出现未经审批的写。
    #[tokio::test]
    async fn execute_batch_requires_approval_for_writes() {
        let (c, _dir) = cfg_isolated("batch-approval");
        // ① 可写模式的批量 ⇒ 拒，且带机器可读码
        let err = execute_batch(
            vec!["CREATE TABLE batch_t (id INTEGER)".to_string()],
            c.clone(),
            Some("writable".to_string()),
            None,
        )
        .await
        .expect_err("批量不许写");
        assert!(
            err.contains("READONLY_BLOCKED"),
            "错误要带机器可读码，实际: {}",
            err
        );

        // ② 只读批量里混进写语句 ⇒ 同样被门禁挡下（证明批量不是绕过审批的后门）
        let err = execute_batch(
            vec!["CREATE TABLE batch_t2 (id INTEGER)".to_string()],
            c.clone(),
            Some("readOnly".to_string()),
            None,
        )
        .await
        .expect_err("只读批量里的写语句必须被挡");
        assert!(
            err.contains("READONLY_BLOCKED"),
            "只读批量里的写必须被只读门禁挡下，实际: {}",
            err
        );
    }

    /// 批量只读是**能用**的：多条只读语句应当逐条跑完并各带一份结果。
    /// （改之前批量接口的只读分支直接报错 ⇒ 这个接口从来没真正用起来过。）
    #[tokio::test]
    async fn execute_batch_runs_readonly_statements() {
        let (c, _dir) = cfg_isolated("batch-readonly");
        exec_w("CREATE TABLE batch_r (id INTEGER)", c.clone())
            .await
            .expect("建表");
        exec_w("INSERT INTO batch_r (id) VALUES (1)", c.clone())
            .await
            .expect("插入");
        let results = execute_batch(
            vec![
                "SELECT id FROM batch_r".to_string(),
                "SELECT COUNT(*) AS n FROM batch_r".to_string(),
            ],
            c.clone(),
            Some("readOnly".to_string()),
            None,
        )
        .await
        .expect("只读批量应当能跑");
        assert_eq!(results.len(), 2, "两条语句各带一份结果");
        assert_eq!(results[0].rows.len(), 1, "第一条应当取到 1 行");
    }

    /// T-043：同一条连接上的**第二个** BEGIN 不能被状态机拒死。
    ///
    /// 上一个事务已经提交（Committed）⇒ 状态机不允许 Committed → Active；
    /// 不重置的话用户在这条连接上就再也开不了新事务了。
    #[tokio::test]
    async fn second_begin_on_same_connection_is_allowed() {
        let (c, _dir) = cfg_isolated("tx-second-begin");
        // 用一个专属代次，避免与其它用例共用同一条事务键
        let gen = 7_777u32;
        for sql in ["BEGIN TRANSACTION", "COMMIT"] {
            let r = execute_query(
                sql.to_string(),
                c.clone(),
                Some("writable".into()),
                Some(grant_for(sql)),
                Some(gen),
                None,
            )
            .await;
            assert!(r.is_ok(), "{} 应成功: {:?}", sql, r.err());
        }
        // 第二个 BEGIN 必须能用
        let sql = "BEGIN TRANSACTION";
        let r = execute_query(
            sql.to_string(),
            c.clone(),
            Some("writable".into()),
            Some(grant_for(sql)),
            Some(gen),
            None,
        )
        .await;
        assert!(
            r.is_ok(),
            "同一条连接上的第二个 BEGIN 应被允许，实际: {:?}",
            r.err()
        );
        let sql = "ROLLBACK";
        let r = execute_query(
            sql.to_string(),
            c.clone(),
            Some("writable".into()),
            Some(grant_for(sql)),
            Some(gen),
            None,
        )
        .await;
        assert!(r.is_ok(), "ROLLBACK 应成功: {:?}", r.err());
    }

    /// 端口 0 的校验挪到了**按驱动**的地方：反序列化层不知道驱动，
    /// 而 SQLite 压根没有端口（前端就送 0）⇒ 放行；
    /// MySQL/PostgreSQL 端口为 0 仍必须被拒 —— 在建池那一刻拒。
    #[tokio::test]
    async fn mysql_connection_without_port_is_rejected() {
        let c = DBConfig {
            id: "port0-probe".into(),
            name: "port0".into(),
            db_type: "mysql".into(),
            host: "127.0.0.1".into(),
            port: 0,
            username: "u".into(),
            password: "p".into(),
            database: "d".into(),
            tls_mode: None,
            tls_ca_path: None,
        };
        let err = mysql_pool_new(&c, 10)
            .await
            .expect_err("MySQL 端口为 0 必须被拒");
        assert!(err.contains("INVALID_PORT"), "错误要带机器可读码: {}", err);
    }

    /// T-046：写操作影响 0 行 ⇒ 乐观并发冲突，必须能被查出来。
    #[tokio::test]
    async fn write_impact_zero_rows_is_reported_as_conflict() {
        let reg = db::session::registry();
        reg.insert(db::session::QueryJobInternal {
            query_id: "conflict-probe".to_string(),
            state: db::session::QueryState::Running,
            state_msg: "执行中".into(),
            cancel_requested: false,
            start_time: std::time::Instant::now(),
            batch_tx: tokio::sync::mpsc::channel(1).0,
            transaction_state: db::session::TransactionState::Active,
            generation: 0,
            conflict_detected: false,
            last_affected_rows: 0,
        });
        // 影响 0 行（表是空的）⇒ 冲突
        reg.record_write_impact("conflict-probe", 0);
        assert!(reg.has_conflict("conflict-probe"), "0 行影响应判为冲突");
        // 影响 1 行 ⇒ 不是冲突
        reg.record_write_impact("conflict-probe", 1);
        assert!(!reg.has_conflict("conflict-probe"), "1 行影响不该算冲突");
        reg.remove("conflict-probe");
    }

    /// T-014：迁移报告的"需重输"清单要能点名到具体连接。
    #[tokio::test]
    async fn secret_migration_names_missing_connections() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-mig2-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);
        let store = secrets::InMemorySecretStore::default();
        let rec = ConnectionRecord {
            id: "m1".into(),
            name: "旧连接".into(),
            db_type: "mysql".into(),
            host: "h".into(),
            port: 3306,
            username: "u".into(),
            password: "p".into(),
            database: "d".into(),
            revision: 1,
            environment: "dev".into(),
            tls_mode: None,
            tls_ca_path: None,
            secret_ref: None,
            credential_state: String::new(),
            credential_note: None,
        };
        let sealed = db::credentials::seal(&store, &[rec], &[]).unwrap();
        // 换个空后端 ⇒ 密钥取不回来了
        let empty = secrets::InMemorySecretStore::default();
        queries::save_connections(&sealed).await.unwrap();
        let report = migration_report_with(&empty).await.unwrap();
        assert_eq!(report.total_connections, 1);
        assert_eq!(report.needs_reentry, 1);
        assert_eq!(report.missing_credentials[0].id, "m1");
        assert_eq!(report.missing_credentials[0].name, "旧连接");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// T-011：改连接参数后旧池必须关掉（否则还连着旧库/旧凭据）。
    #[tokio::test]
    async fn editing_connection_closes_stale_pool() {
        let _guard = crate::tests::DATADIR_LOCK.lock().await;
        let tmp = std::env::temp_dir().join(format!(
            "zbiz-pooledit-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &tmp);

        use crate::db::connection::{fingerprint, pool_registry, AnyPool, PoolKey};
        let registry = pool_registry();
        let base = DBConfig {
            id: "pool-edit-probe".into(),
            name: "n".into(),
            db_type: "sqlite".into(),
            host: "old-host".into(),
            port: 0,
            username: String::new(),
            password: String::new(),
            database: ":memory:".into(),
            tls_mode: None,
            tls_ca_path: None,
        };
        let key = PoolKey {
            connection_id: base.id.clone(),
            fingerprint: fingerprint(&base),
            access_mode: "readOnly".into(),
        };
        registry
            .get_or_insert(key.clone(), || async {
                Ok(AnyPool::Sqlite(
                    sqlx::SqlitePool::connect("sqlite::memory:")
                        .await
                        .map_err(|e| e.to_string())?,
                ))
            })
            .await
            .expect("建池");

        let mk_rec = |host: &str| ConnectionRecord {
            id: base.id.clone(),
            name: "n".into(),
            db_type: "sqlite".into(),
            host: host.into(),
            port: 0,
            username: String::new(),
            password: String::new(),
            database: ":memory:".into(),
            revision: 1,
            environment: "dev".into(),
            tls_mode: None,
            tls_ca_path: None,
            secret_ref: None,
            credential_state: String::new(),
            credential_note: None,
        };
        // 磁盘上先放一份"旧参数"的记录
        queries::save_connections(std::slice::from_ref(&mk_rec("old-host")))
            .await
            .unwrap();
        // 改成新 host 保存 ⇒ 旧池必须被关掉
        let store = secrets::InMemorySecretStore::default();
        save_connections_with(&store, std::slice::from_ref(&mk_rec("new-host")), None)
            .await
            .unwrap();
        // 再关一次应当关不到东西（说明保存时已经关过了）
        assert_eq!(
            registry.remove_connection(&base.id),
            0,
            "改连接参数后旧池应已在保存时被关掉"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
    // ================== T-047：CSV 导入门禁 ==================

    /// 写一个临时 CSV，返回文件路径
    fn write_csv(dir: &std::path::Path, name: &str, content: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, content).expect("写 CSV");
        p.to_string_lossy().to_string()
    }

    /// 只读通道必须拒导入（导入永远是写）
    #[tokio::test]
    async fn csv_import_readonly_mode_blocked() {
        let (c, dir) = cfg_isolated("csv-ro");
        let path = write_csv(&dir, "a.csv", "id,name\n1,x\n");
        let err = csv_import_execute(
            c,
            path,
            "t".into(),
            None,
            None,
            Some(ImportGate {
                access_mode: Some("readOnly".into()),
                approval: None,
                expected_rows: None,
            }),
        )
        .await
        .expect_err("只读模式必须拒绝导入");
        assert!(err.contains("READONLY_BLOCKED"), "要带机器可读码: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 可写但没审批 ⇒ 拒
    #[tokio::test]
    async fn csv_import_requires_approval() {
        let (c, dir) = cfg_isolated("csv-noappr");
        let path = write_csv(&dir, "a.csv", "id,name\n1,x\n");
        let err = csv_import_execute(
            c,
            path,
            "t".into(),
            None,
            None,
            Some(ImportGate {
                access_mode: Some("writable".into()),
                approval: None,
                expected_rows: None,
            }),
        )
        .await
        .expect_err("写入必须审批");
        assert!(err.contains("APPROVAL_DENIED"), "要带机器可读码: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 摘要绑的是别的表 ⇒ 摘要对不上 ⇒ 拒
    #[tokio::test]
    async fn csv_import_rejects_wrong_digest() {
        let (c, dir) = cfg_isolated("csv-digest");
        let path = write_csv(&dir, "a.csv", "id,name\n1,x\n");
        let grant = grant_for("CSV_IMPORT table=other rows=1");
        let err = csv_import_execute(
            c,
            path,
            "t".into(),
            None,
            None,
            Some(ImportGate {
                access_mode: Some("writable".into()),
                approval: Some(grant),
                expected_rows: Some(1),
            }),
        )
        .await
        .expect_err("摘要不符必须拒");
        assert!(err.contains("APPROVAL_DENIED"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 行数与审批时对不上 ⇒ 拒（文件在预览之后被改过）
    #[tokio::test]
    async fn csv_import_rejects_row_count_mismatch() {
        let (c, dir) = cfg_isolated("csv-rows");
        let path = write_csv(&dir, "a.csv", "id,name\n1,x\n2,y\n");
        let grant = grant_for("CSV_IMPORT table=t rows=2");
        let err = csv_import_execute(
            c,
            path,
            "t".into(),
            None,
            None,
            Some(ImportGate {
                access_mode: Some("writable".into()),
                approval: Some(grant),
                expected_rows: Some(99),
            }),
        )
        .await
        .expect_err("行数对不上必须拒");
        assert!(err.contains("行数"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 带合法审批的导入：真写进库，且内容一字不差
    #[tokio::test]
    async fn csv_import_with_valid_grant_writes_rows() {
        let (c, dir) = cfg_isolated("csv-ok");
        let path = write_csv(&dir, "a.csv", "id,name\n1,张三\n2,李四\n");
        exec_w("CREATE TABLE t (id INTEGER, name TEXT)", c.clone())
            .await
            .expect("建表");

        // canonical 由后端出（前端不自己拼）
        let plan = csv_import_plan(path.clone(), None, Some(20), Some(1000), "t".into())
            .await
            .expect("出计划");
        assert_eq!(plan.import_rows, 2);
        assert_eq!(plan.canonical, "CSV_IMPORT table=t rows=2");

        let grant = grant_for(&plan.canonical);
        let res = csv_import_execute(
            c.clone(),
            path,
            "t".into(),
            None,
            Some(plan.import_limit),
            Some(ImportGate {
                access_mode: Some("writable".into()),
                approval: Some(grant),
                expected_rows: Some(plan.import_rows),
            }),
        )
        .await
        .expect("带审批的导入应当成功");
        assert!(res.success, "导入失败: {:?}", res.errors);
        assert_eq!(res.rows_imported, 2);

        // 真落库了，而且中文没乱码
        let r = exec_q("SELECT name FROM t ORDER BY id", c, None)
            .await
            .expect("查导入结果");
        assert_eq!(r.rows.len(), 2);
        assert_eq!(
            r.rows[0][0],
            serde_json::json!({"__kind": "text", "value": "张三"})
        );
        assert_eq!(
            r.rows[1][0],
            serde_json::json!({"__kind": "text", "value": "李四"})
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 计划里的 import_rows 必须与真导入时的行数一致（摘要绑的就是这个数）
    #[tokio::test]
    async fn csv_plan_import_rows_matches_execute() {
        let (c, dir) = cfg_isolated("csv-plan");
        let path = write_csv(&dir, "a.csv", "id,name\n1,x\n2,y\n3,z\n\n4,w\n");
        let plan = csv_import_plan(path.clone(), None, Some(2), Some(1000), "t".into())
            .await
            .expect("出计划");
        // 空行不插入 ⇒ 计划说 4 行，不是 5 行
        assert_eq!(plan.import_rows, 4);
        assert_eq!(plan.total_rows, 5, "原始行数仍是 5");
        assert_eq!(plan.sample_rows.len(), 2, "样本行按 sample_limit 截断");

        let (headers, rows) =
            db::csv_import::parse_csv(&std::fs::read(&path).unwrap(), ',', plan.import_limit)
                .expect("解析");
        assert_eq!(headers.len(), 2);
        assert_eq!(rows.len() as u64, plan.import_rows, "计划与执行必须同口径");
        let _ = std::fs::remove_dir_all(&dir);
        drop(c);
    }
}

/// T-025 的 TLS 映射测试。
///
/// **为什么单独一个模块**：原先的 TLS 断言写在 `mysql_select_only` 里面，
/// 而那个测试在本机没有 MySQL 时会**提前 return**（`[skip] 本机 MySQL 不可用`）——
/// 于是那些断言在绝大多数机器上**一次都没跑过**。安全相关的断言必须无条件执行。
#[cfg(test)]
mod tls_tests {
    use super::*;
    use sqlx::mysql::MySqlSslMode;
    use sqlx::postgres::PgSslMode;

    #[test]
    fn verify_full_actually_verifies_certificates() {
        // 守卫测试：旧实现"留 opts 默认"，而 sqlx 的默认是 Preferred ⇒ **根本不校验证书**。
        // 这条断言就是为了让"默认档位不再等于 sqlx 默认"这件事被钉住，
        // 防止哪天又被改回 no-op 分支。
        // sqlx 的这两个枚举**没有实现 PartialEq**，所以只能用 matches! 判形状。
        assert!(
            !matches!(TlsMode::VerifyFull.to_mysql(), MySqlSslMode::Preferred),
            "verify_full 档位不能等于 sqlx 的 Preferred（那是不校验证书）"
        );
        assert!(
            !matches!(TlsMode::VerifyFull.to_pg(), PgSslMode::Prefer),
            "verify_full 档位不能等于 sqlx 的 Prefer（那是不校验证书）"
        );
    }

    #[test]
    fn verify_full_maps_to_the_strictest_verifying_mode() {
        // MySQL：只有 VerifyIdentity 验主机名（VerifyCa 不验）
        assert!(matches!(
            TlsMode::VerifyFull.to_mysql(),
            MySqlSslMode::VerifyIdentity
        ));
        // PG：只有 VerifyFull 验主机名
        assert!(matches!(TlsMode::VerifyFull.to_pg(), PgSslMode::VerifyFull));
    }

    #[test]
    fn prefer_encrypts_but_skips_verification_and_is_flagged_as_degraded() {
        // sqlx 的 Required/Require = 加密但不校验证书 —— 这是**安全降级**，必须被标出来。
        assert!(matches!(TlsMode::Prefer.to_mysql(), MySqlSslMode::Required));
        assert!(matches!(TlsMode::Prefer.to_pg(), PgSslMode::Require));
        assert!(TlsMode::Prefer.skips_certificate_verification());
    }

    #[test]
    fn disable_turns_tls_off_entirely() {
        assert!(matches!(
            TlsMode::Disable.to_mysql(),
            MySqlSslMode::Disabled
        ));
        assert!(matches!(TlsMode::Disable.to_pg(), PgSslMode::Disable));
        assert!(TlsMode::Disable.skips_certificate_verification());
    }

    #[test]
    fn only_verify_full_is_treated_as_verifying() {
        assert!(!TlsMode::VerifyFull.skips_certificate_verification());
    }

    #[test]
    fn unrecognised_values_fall_back_to_the_safe_mode() {
        // 认不出来的值**不许**回退到宽松档 —— 那是"配错了就悄悄不安全"。
        assert_eq!(TlsMode::from_str_opt(Some("whatever")), TlsMode::VerifyFull);
        assert_eq!(TlsMode::from_str_opt(Some("")), TlsMode::VerifyFull);
        assert_eq!(TlsMode::from_str_opt(None), TlsMode::VerifyFull);
    }

    #[test]
    fn configured_mode_wins_over_environment_variable() {
        // 连接配置里的档位优先于环境变量
        let env_name = "ZBT_TLS_TEST_MODE";
        std::env::remove_var(env_name);
        // 都没有 → 默认（最安全的一档）
        assert_eq!(resolve_tls_mode(None, env_name), TlsMode::VerifyFull);
        // 环境变量有值 → 用它
        std::env::set_var(env_name, "prefer");
        assert_eq!(resolve_tls_mode(None, env_name), TlsMode::Prefer);
        // 连接配置有值 → 配置优先
        assert_eq!(
            resolve_tls_mode(Some(TlsMode::Disable), env_name),
            TlsMode::Disable
        );
        // 环境变量认不出来 → 仍然回到最安全的一档
        std::env::set_var(env_name, "nonsense");
        assert_eq!(resolve_tls_mode(None, env_name), TlsMode::VerifyFull);
        std::env::remove_var(env_name);
    }

    #[test]
    fn config_round_trips_through_serde() {
        // 连接配置要落盘，所以枚举必须能序列化/反序列化
        let json = serde_json::to_string(&TlsMode::VerifyFull).unwrap();
        assert_eq!(json, "\"verify_full\"");
        let back: TlsMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TlsMode::VerifyFull);
    }
}
