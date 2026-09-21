// T-031：查询会话与流式结果管理
//
// 简化实现：使用 std::sync::OnceLock 替代 once_cell，
// 不做独立 actor，而是通过查询 ID 跟踪活跃作业。
//
// 核心功能：
// 1. QuerySession 跟踪会话状态（connectionId, generation, schema, accessMode）
// 2. 流式执行：通过 query_start + query_next 实现分批拉取
// 3. 背压：每批最多 MAX_BATCH_ROWS 行，前端消费后才拉下一批

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc;

/// T-031 常量配置
pub const MAX_BATCH_ROWS: usize = 500;
pub const MAX_BATCH_BYTES: usize = 1 * 1024 * 1024; // 1 MiB
pub const MAX_QUEUE_BATCHES: usize = 2;

/// 查询状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryState {
    Queued,
    Running,
    Streaming,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

impl QueryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueryState::Queued => "queued",
            QueryState::Running => "running",
            QueryState::Streaming => "streaming",
            QueryState::CancelRequested => "cancel_requested",
            QueryState::Succeeded => "succeeded",
            QueryState::Failed => "failed",
            QueryState::Cancelled => "cancelled",
            QueryState::Unknown => "unknown",
        }
    }
}

/// 单个结果批次
#[derive(Debug, Clone)]
pub struct QueryBatch {
    pub seq: u32,
    pub columns: Vec<crate::ColumnMeta>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub truncated: bool,
    pub affected_rows: u64,
}

/// 查询作业信息
#[derive(Debug, Clone)]
pub struct QueryJobInternal {
    pub query_id: String,
    pub state: QueryState,
    pub state_msg: String,
    pub cancel_requested: bool,
    pub start_time: Instant,
    pub batch_tx: mpsc::Sender<QueryBatch>,
}

/// 活跃查询注册表（进程内单例）
pub struct QueryRegistry {
    inner: Mutex<HashMap<String, QueryJobInternal>>,
}

impl QueryRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn insert(&self, job: QueryJobInternal) {
        self.inner.lock().expect("registry lock").insert(job.query_id.clone(), job);
    }

    pub fn get_state(&self, query_id: &str) -> Option<(QueryState, String)> {
        self.inner.lock().expect("registry lock")
            .get(query_id)
            .map(|j| (j.state, j.state_msg.clone()))
    }

    pub fn mark_succeeded(&self, query_id: &str) {
        if let Some(job) = self.inner.lock().expect("registry lock").get_mut(query_id) {
            job.state = QueryState::Succeeded;
            job.state_msg = "查询完成".into();
        }
    }

    pub fn mark_failed(&self, query_id: &str, msg: &str) {
        if let Some(job) = self.inner.lock().expect("registry lock").get_mut(query_id) {
            job.state = QueryState::Failed;
            job.state_msg = msg.to_string();
        }
    }

    pub fn mark_cancel_requested(&self, query_id: &str) {
        if let Some(job) = self.inner.lock().expect("registry lock").get_mut(query_id) {
            job.cancel_requested = true;
            job.state = QueryState::CancelRequested;
        }
    }

    pub fn pending_count(&self) -> usize {
        self.inner.lock().expect("registry lock")
            .values()
            .filter(|j| !matches!(
                j.state,
                QueryState::Succeeded | QueryState::Failed
                    | QueryState::Cancelled | QueryState::Unknown
            ))
            .count()
    }

    pub fn remove(&self, query_id: &str) {
        self.inner.lock().expect("registry lock").remove(query_id);
    }
}

static REGISTRY: OnceLock<QueryRegistry> = OnceLock::new();

pub fn registry() -> &'static QueryRegistry {
    REGISTRY.get_or_init(QueryRegistry::new)
}

/// 全局计数器用于生成唯一 queryId
static QUERY_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn next_query_id() -> String {
    let seq = QUERY_SEQ.fetch_add(1, Ordering::SeqCst);
    format!("q-{}-{}", chrono::Utc::now().timestamp_millis(), seq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn query_state_str_roundtrip() {
        assert_eq!(QueryState::Running.as_str(), "running");
        assert_eq!(QueryState::Streaming.as_str(), "streaming");
        assert_eq!(QueryState::Cancelled.as_str(), "cancelled");
    }

    #[test]
    fn query_batch_defaults() {
        let batch = QueryBatch {
            seq: 0,
            columns: vec![],
            rows: vec![],
            truncated: false,
            affected_rows: 0,
        };
        assert!(!batch.truncated);
        assert_eq!(batch.affected_rows, 0);
    }

    #[test]
    fn query_registry_state_tracking() {
        let reg = QueryRegistry::new();
        let (tx, _rx) = mpsc::channel(1);
        let job = QueryJobInternal {
            query_id: "q1".into(),
            state: QueryState::Queued,
            state_msg: "queued".into(),
            cancel_requested: false,
            start_time: Instant::now(),
            batch_tx: tx,
        };
        reg.insert(job);
        assert_eq!(reg.get_state("q1").unwrap().0, QueryState::Queued);
        reg.mark_succeeded("q1");
        assert_eq!(reg.get_state("q1").unwrap().0, QueryState::Succeeded);
    }

    #[test]
    fn next_query_id_unique() {
        let id1 = next_query_id();
        let id2 = next_query_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("q-"));
    }

    #[test]
    fn registry_remove_cleans_up() {
        let reg = QueryRegistry::new();
        let (tx, _rx) = mpsc::channel(1);
        let job = QueryJobInternal {
            query_id: "q2".into(),
            state: QueryState::Running,
            state_msg: "running".into(),
            cancel_requested: false,
            start_time: Instant::now(),
            batch_tx: tx,
        };
        reg.insert(job);
        assert!(reg.get_state("q2").is_some());
        reg.remove("q2");
        assert!(reg.get_state("q2").is_none());
    }
}
