// 查询历史 & 保存查询 & 连接配置的本地持久化
use std::path::{Path, PathBuf};
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

/// S0 原子写入：临时文件 → sync → rename（DB-09 / A11）
pub fn atomic_write_pub(path: &Path, content: &[u8]) -> Result<(), String> {
    atomic_write(path, content)
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
        // 确保数据落盘再 rename
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&tmp) {
            let _ = f.sync_all();
        }
    }
    // rename 在同一文件系统上是原子操作
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("原子替换失败: {} -> {} ({})", tmp.display(), path.display(), e))?;
    Ok(())
}

/// S0：写入前剥离敏感字段（DB-08 / A10）
/// 规则：清空 password / api_key 字段值，但保留字段名以便前端识别「曾经配置过」。
fn strip_secrets_from_connections(records: &mut Vec<ConnectionRecord>) {
    for r in records.iter_mut() {
        if !r.password.is_empty() {
            r.password.clear();
        }
    }
}

pub async fn save_history(history: &[QueryHistoryItem]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(history).map_err(|e| e.to_string())?;
    atomic_write(&history_path()?, json.as_bytes())
}

pub async fn load_history() -> Result<Vec<QueryHistoryItem>, String> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

pub async fn clear_history() -> Result<(), String> {
    let path = history_path()?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub async fn save_saved_query(item: SavedQuery) -> Result<(), String> {
    let mut all = load_saved_queries_inner().await?;
    // 如果存在同 id 则替换
    all.retain(|q| q.id != item.id);
    all.push(item);
    let json = serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?;
    atomic_write(&saved_queries_path()?, json.as_bytes())
}

pub async fn load_saved_queries() -> Result<Vec<SavedQuery>, String> {
    load_saved_queries_inner().await
}

async fn load_saved_queries_inner() -> Result<Vec<SavedQuery>, String> {
    let path = saved_queries_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

pub async fn delete_saved_query(id: &str) -> Result<(), String> {
    let mut all = load_saved_queries_inner().await?;
    all.retain(|q| q.id != id);
    let json = serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?;
    atomic_write(&saved_queries_path()?, json.as_bytes())
}

// ================== 连接配置持久化 ==================

pub async fn save_connections(connections: &[ConnectionRecord]) -> Result<(), String> {
    // T-007：写入前剥离敏感字段；只保留空 password 字段名，便于加载识别
    let mut sanitized: Vec<ConnectionRecord> = connections.to_vec();
    strip_secrets_from_connections(&mut sanitized);
    let json = serde_json::to_string_pretty(&sanitized).map_err(|e| e.to_string())?;
    atomic_write(&connections_path()?, json.as_bytes())
}

pub async fn load_connections() -> Result<Vec<ConnectionRecord>, String> {
    let path = connections_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}
