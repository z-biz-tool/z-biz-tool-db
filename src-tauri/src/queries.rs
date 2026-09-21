// 查询历史 & 保存查询 & 连接配置的本地持久化
//
// T-016 信封结构：所有持久化文件均采用 envelope 结构
// `{ schemaVersion, revision, updatedAt, payload, checksum }`，
// 加载时校验 schemaVersion 与 checksum；不匹配则返回 Err，
// 防止加载到损坏数据后误存空数组（A12 / DB-09）。
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

use crate::{QueryHistoryItem, SavedQuery, ConnectionRecord};

/// 数据目录获取
/// S0 修复：失败时返回 Err 而非静默 `.ok()`（DB-09）。
pub fn get_data_dir() -> Result<PathBuf, String> {
    if let Ok(custom) = std::env::var("Z_BIZ_TOOL_DB_DATA_DIR") {
        let d = PathBuf::from(custom);
        std::fs::create_dir_all(&d)
            .map_err(|e| format!("创建自定义数据目录失败: {} ({})", d.display(), e))?;
        return Ok(d);
    }
    let mut dir = dirs::data_local_dir()
        .ok_or_else(|| "无法解析本地数据目录；请设置 Z_BIZ_TOOL_DB_DATA_DIR 环境变量".to_string())?;
    dir.push("z-biz-tool-db");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建默认数据目录失败: {} ({})", dir.display(), e))?;
    Ok(dir)
}

fn data_dir() -> Result<PathBuf, String> {
    get_data_dir()
}

fn connections_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("connections.json"))
}

fn history_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("query_history.json"))
}

fn saved_queries_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("saved_queries.json"))
}

/// 当前 schema 版本号；后续迁移递增
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// T-016 信封结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedEnvelope {
    /// schema 版本（递增触发迁移分支）
    pub schema_version: u32,
    /// 单调递增修订号；用于跨实例 CAS 比对
    pub revision: u64,
    /// ISO 时间戳（UNIX 秒）
    pub updated_at: i64,
    /// 业务数据；保留为 Value 以让序列化/反序列化自洽
    pub payload: serde_json::Value,
    /// payload 序列化后指纹（仅检错，不作为防篡改签名）
    pub checksum: String,
}

/// 计算 envelope 载荷的指纹（仅检错，不作为防篡改签名）。
/// 使用 std::collections::hash_map::DefaultHasher，避免引入新依赖；
/// 文档明确"checksum 只检错，不作为防篡改签名"。
fn payload_checksum<T: Serialize>(payload: &T) -> Result<String, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let bytes = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let mut hasher = DefaultHasher::new();
    hasher.write_usize(bytes.len());
    hasher.write(&bytes);
    let digest = hasher.finish();
    Ok(format!("{:016x}", digest))
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("目标路径无父目录: {}", path.display()))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("创建目录失败: {} ({})", dir.display(), e))?;
    let tmp = dir.join(format!(
        ".{}.swap.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file"),
        std::process::id()
    ));
    {
        std::fs::write(&tmp, content)
            .map_err(|e| format!("写临时文件失败: {} ({})", tmp.display(), e))?;
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&tmp) {
            let _ = f.sync_all();
        }
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("原子替换失败: {} -> {} ({})", tmp.display(), path.display(), e))?;
    Ok(())
}

/// 公共入口：供 save_ai_config 等模块复用原子写能力
pub fn atomic_write_pub(path: &Path, content: &[u8]) -> Result<(), String> {
    atomic_write(path, content)
}

/// T-061：升级前自动备份上一版本
/// 仅在保存时若旧文件存在并能解析为 envelope 且 schema_version 不同则备份。
/// 备份路径：`{path}.bak.v{prev_schema_version}`；成功完成后才写新文件。
fn backup_previous_version(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let env: PersistedEnvelope = match serde_json::from_str(&content) {
        Ok(e) => e,
        Err(_) => return Ok(()), // 旧格式无法解析不阻止新版写入
    };
    let prev = env.schema_version;
    if prev >= CURRENT_SCHEMA_VERSION {
        return Ok(()); // 已是新版本或更新，无需备份
    }
    let backup_path = path.with_extension(format!(
        "{}.bak.v{}",
        path.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("json"),
        prev
    ));
    if !backup_path.exists() {
        std::fs::copy(path, &backup_path).map_err(|e| {
            format!("升级前备份失败: {} -> {} ({})", path.display(), backup_path.display(), e)
        })?;
    }
    Ok(())
}

/// 通用 envelope 保存与加载
/// - 加载时校验 schema_version 与 checksum；任意不匹配返回 Err
/// - 兼容旧 v1（无 envelope 的纯数组）升级路径：检测到非 envelope 自动迁移
fn save_envelope<T>(path: &Path, payload: &T, _expected_max_version: u32) -> Result<(), String>
where
    T: Serialize,
{
    // T-061：先备份旧版本（如果 schema 升级），再写新版本
    backup_previous_version(path)?;

    let payload_value = serde_json::to_value(payload).map_err(|e| e.to_string())?;
    let checksum = payload_checksum(&payload_value)?;
    let env = PersistedEnvelope {
        schema_version: CURRENT_SCHEMA_VERSION,
        revision: next_revision_for(path)?,
        updated_at: chrono::Utc::now().timestamp(),
        checksum,
        payload: payload_value,
    };
    let json = serde_json::to_string_pretty(&env).map_err(|e| e.to_string())?;
    atomic_write(path, json.as_bytes())?;
    let _ = _expected_max_version;
    Ok(())
}

fn load_envelope<T>(path: &Path, expected_max_version: u32) -> Result<Option<T>, String>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;

    // 1) 尝试 envelope
    if let Ok(env) = serde_json::from_str::<PersistedEnvelope>(&content) {
        if env.schema_version > expected_max_version {
            return Err(format!(
                "持久化 schema_version {} 高于当前支持的 {}，拒绝读取以保护数据",
                env.schema_version, expected_max_version
            ));
        }
        // 校验 checksum
        let expected = payload_checksum(&env.payload)?;
        if expected != env.checksum {
            return Err(format!(
                "校验和不匹配：文件可能已损坏（expected={}, got={}）",
                expected, env.checksum
            ));
        }
        // 反序列化 payload 为 T
        return serde_json::from_value::<T>(env.payload).map(Some).map_err(|e| {
            format!("envelope payload 反序列化为目标类型失败: {}", e)
        });
    }

    // 2) 兼容 v1：裸数组/T，按迁移分支迁移到 v2 envelope
    match serde_json::from_str::<T>(&content) {
        Ok(payload) => {
            save_envelope(path, &payload, CURRENT_SCHEMA_VERSION)?;
            Ok(Some(payload))
        }
        Err(e) => Err(format!(
            "持久化文件损坏且无法迁移：{}；请手动检查 {:?}",
            e, path
        )),
    }
}

/// 单调递增修订号：持久化在 .rev 文件；首次读 0
fn revision_path(target: &Path) -> PathBuf {
    let mut p = target.to_path_buf();
    let ext = target
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("json");
    p.set_extension(format!("{}.rev", ext));
    p
}

fn next_revision_for(path: &Path) -> Result<u64, String> {
    let rev_path = revision_path(path);
    let prev = if rev_path.exists() {
        std::fs::read_to_string(&rev_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
    } else {
        0
    };
    let next = prev.saturating_add(1);
    std::fs::write(&rev_path, next.to_string()).map_err(|e| e.to_string())?;
    Ok(next)
}


/// S0：写入前剥离敏感字段（DB-08 / A10）
fn strip_secrets_from_connections(records: &mut Vec<ConnectionRecord>) {
    for r in records.iter_mut() {
        if !r.password.is_empty() {
            r.password.clear();
        }
    }
}

// ================== 历史 ==================

pub async fn save_history(history: &[QueryHistoryItem]) -> Result<(), String> {
    save_envelope(&history_path()?, &history.to_vec(), CURRENT_SCHEMA_VERSION)
}

pub async fn load_history() -> Result<Vec<QueryHistoryItem>, String> {
    match load_envelope::<Vec<QueryHistoryItem>>(&history_path()?, CURRENT_SCHEMA_VERSION)? {
        Some(payload) => Ok(payload),
        None => Ok(Vec::new()),
    }
}

pub async fn clear_history() -> Result<(), String> {
    let path = history_path()?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    let rev = revision_path(&path);
    if rev.exists() {
        let _ = std::fs::remove_file(rev);
    }
    Ok(())
}

// ================== 收藏 ==================

pub async fn save_saved_query(item: SavedQuery) -> Result<(), String> {
    let mut all = load_saved_queries_inner().await?;
    all.retain(|q| q.id != item.id);
    all.push(item);
    save_envelope(&saved_queries_path()?, &all, CURRENT_SCHEMA_VERSION)
}

pub async fn load_saved_queries() -> Result<Vec<SavedQuery>, String> {
    load_saved_queries_inner().await
}

async fn load_saved_queries_inner() -> Result<Vec<SavedQuery>, String> {
    match load_envelope::<Vec<SavedQuery>>(&saved_queries_path()?, CURRENT_SCHEMA_VERSION)? {
        Some(payload) => Ok(payload),
        None => Ok(Vec::new()),
    }
}

pub async fn delete_saved_query(id: &str) -> Result<(), String> {
    let mut all = load_saved_queries_inner().await?;
    all.retain(|q| q.id != id);
    save_envelope(&saved_queries_path()?, &all, CURRENT_SCHEMA_VERSION)
}

// ================== 连接配置 ==================

pub async fn save_connections(connections: &[ConnectionRecord]) -> Result<(), String> {
    let mut sanitized: Vec<ConnectionRecord> = connections.to_vec();
    strip_secrets_from_connections(&mut sanitized);
    save_envelope(&connections_path()?, &sanitized, CURRENT_SCHEMA_VERSION)
}

pub async fn load_connections() -> Result<Vec<ConnectionRecord>, String> {
    match load_envelope::<Vec<ConnectionRecord>>(&connections_path()?, CURRENT_SCHEMA_VERSION)? {
        Some(payload) => Ok(payload),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fresh_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zbiz-queries-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn envelope_roundtrip_preserves_payload() {
        let path = fresh_path("env-rt.json");
        save_envelope(&path, &vec![1u32, 2, 3], CURRENT_SCHEMA_VERSION).unwrap();
        let loaded = load_envelope::<Vec<u32>>(&path, CURRENT_SCHEMA_VERSION).unwrap();
        let payload = loaded.unwrap();
        assert_eq!(payload, vec![1, 2, 3]);
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let path = fresh_path("env-corrupt.json");
        save_envelope(&path, &vec!["a".to_string(), "b".to_string()], CURRENT_SCHEMA_VERSION).unwrap();
        // 手工注入坏 checksum
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["checksum"] = serde_json::Value::String("deadbeef00000000".into());
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        let res = load_envelope::<Vec<String>>(&path, CURRENT_SCHEMA_VERSION);
        assert!(res.is_err(), "checksum 不匹配应拒: {:?}", res);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_higher_schema_rejected() {
        let path = fresh_path("env-higher.json");
        let env = PersistedEnvelope {
            schema_version: CURRENT_SCHEMA_VERSION + 5,
            revision: 1,
            updated_at: 0,
            checksum: "x".into(),
            payload: serde_json::json!({}),
        };
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        let res = load_envelope::<serde_json::Value>(&path, CURRENT_SCHEMA_VERSION);
        assert!(res.is_err(), "更高 schema 应被拒以保护数据");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn upgrade_bak_v1_when_saving_v2() {
        let path = std::env::temp_dir().join(format!(
            "zbiz-upg-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        let target = path.join("connections.json");
        // 写一个旧 envelope，schema_version=1
        let old = PersistedEnvelope {
            schema_version: 1,
            revision: 1,
            updated_at: 0,
            checksum: "0".repeat(16),
            payload: serde_json::json!([1, 2, 3]),
        };
        std::fs::write(&target, serde_json::to_string(&old).unwrap()).unwrap();
        // 升级到 current schema
        save_envelope::<Vec<i32>>(&target, &vec![10, 20], CURRENT_SCHEMA_VERSION).unwrap();
        // 备份文件应存在
        let backup = target.with_extension("json.bak.v1");
        assert!(backup.exists(), "升级前备份未生成: {:?}", backup);
        let loaded = load_envelope::<Vec<i32>>(&target, CURRENT_SCHEMA_VERSION)
            .unwrap()
            .unwrap();
        assert_eq!(loaded, vec![10, 20]);
        let _ = std::fs::remove_dir_all(&path);
    }
    #[test]
    fn legacy_v1_migrates_to_envelope() {
        let path = fresh_path("env-legacy.json");
        // 直接写入 v1 格式（裸数组）
        std::fs::write(&path, "[1,2,3,4]").unwrap();
        let payload = load_envelope::<Vec<i32>>(&path, CURRENT_SCHEMA_VERSION).unwrap().unwrap();
        assert_eq!(payload, vec![1, 2, 3, 4]);
        // 二次加载应命中 envelope
        let payload2 = load_envelope::<Vec<i32>>(&path, CURRENT_SCHEMA_VERSION).unwrap().unwrap();
        assert_eq!(payload2, vec![1, 2, 3, 4]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_payload_returns_error() {
        let path = fresh_path("env-broken.json");
        std::fs::write(&path, "this is not json at all").unwrap();
        let res = load_envelope::<Vec<i32>>(&path, CURRENT_SCHEMA_VERSION);
        assert!(res.is_err(), "完全损坏文件应报错而非静默空数组");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn revision_increments_on_save() {
        let path = fresh_path("env-rev.json");
        save_envelope::<HashMap<String, i32>>(&path, &HashMap::new(), CURRENT_SCHEMA_VERSION).unwrap();
        let r1 = next_revision_for(&path).unwrap();
        let r2 = next_revision_for(&path).unwrap();
        assert!(r2 > r1, "revision 应单调递增: {} {}", r1, r2);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(revision_path(&path));
    }
}
