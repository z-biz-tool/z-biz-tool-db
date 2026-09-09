// 查询历史 & 保存查询的本地持久化
use crate::{QueryHistoryItem, SavedQuery};
use std::path::PathBuf;

fn data_dir() -> PathBuf {
    let mut dir = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push("z-biz-tool-db");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn history_path() -> PathBuf {
    let mut p = data_dir();
    p.push("query_history.json");
    p
}

fn saved_queries_path() -> PathBuf {
    let mut p = data_dir();
    p.push("saved_queries.json");
    p
}

pub async fn save_history(history: &[QueryHistoryItem]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(history).map_err(|e| e.to_string())?;
    std::fs::write(history_path(), json).map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn load_history() -> Result<Vec<QueryHistoryItem>, String> {
    let path = history_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

pub async fn clear_history() -> Result<(), String> {
    let path = history_path();
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
    std::fs::write(saved_queries_path(), json).map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn load_saved_queries() -> Result<Vec<SavedQuery>, String> {
    load_saved_queries_inner().await
}

async fn load_saved_queries_inner() -> Result<Vec<SavedQuery>, String> {
    let path = saved_queries_path();
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
    std::fs::write(saved_queries_path(), json).map_err(|e| e.to_string())?;
    Ok(())
}