// T-031：查询会话与流式结果管理
//
// 简化实现：使用 std::sync::OnceLock 替代 once_cell，
// 不做独立 actor，而是通过查询 ID 跟踪活跃作业。
//
// 核心功能：
// 1. QuerySession 跟踪会话状态（connectionId, generation, schema, accessMode）
// 2. 流式执行：通过 query_start + query_next 实现分批拉取
// 3. 背压：每批最多 MAX_BATCH_ROWS 行，前端消费后才拉下一批
// 4. T-043：事务状态机 idle → active → failed/committing → committed/rolledBack/unknown

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

/// T-043：事务状态机
/// idle → active → failed/committing → committed/rolledBack/unknown
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    Idle,
    Active,
    Failed,
    Committing,
    Committed,
    RollingBack,
    RolledBack,
    Unknown,
}

impl TransactionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransactionState::Idle => "idle",
            TransactionState::Active => "active",
            TransactionState::Failed => "failed",
            TransactionState::Committing => "committing",
            TransactionState::Committed => "committed",
            TransactionState::RollingBack => "rolling_back",
            TransactionState::RolledBack => "rolled_back",
            TransactionState::Unknown => "unknown",
        }
    }

    /// 校验状态转换是否合法
    pub fn can_transition(&self, to: &TransactionState) -> bool {
        matches!(
            (self, to),
            (TransactionState::Idle, TransactionState::Active)
                | (TransactionState::Active, TransactionState::Failed)
                | (TransactionState::Active, TransactionState::Committing)
                | (TransactionState::Active, TransactionState::RollingBack)
                | (TransactionState::Committing, TransactionState::Committed)
                | (TransactionState::Committing, TransactionState::Unknown)
                | (TransactionState::RollingBack, TransactionState::RolledBack)
                | (TransactionState::RollingBack, TransactionState::Unknown)
                | (TransactionState::Failed, TransactionState::Idle)
                | (TransactionState::RolledBack, TransactionState::Idle)
                | (_, TransactionState::Unknown) // 任何状态都可以转到 unknown
        )
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
    /// T-043：事务状态
    pub transaction_state: TransactionState,
    /// T-043：事务所属 generation（用于校验同一事务操作在同一 generation）
    pub generation: u32,
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

    // T-043：获取事务状态
    pub fn get_transaction_state(&self, query_id: &str) -> Option<TransactionState> {
        self.inner.lock().expect("registry lock")
            .get(query_id)
            .map(|j| j.transaction_state)
    }

    // T-043：BEGIN 事务 - idle → active
    pub fn begin_transaction(&self, query_id: &str, generation: u32) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("registry lock");
        let job = guard.get_mut(query_id)
            .ok_or_else(|| format!("查询 {} 不存在", query_id))?;
        if job.generation != generation {
            return Err(format!(
                "generation 不匹配：期望 {}，实际 {}",
                generation, job.generation
            ));
        }
        if !job.transaction_state.can_transition(&TransactionState::Active) {
            return Err(format!(
                "事务状态 {} 不允许 BEGIN",
                job.transaction_state.as_str()
            ));
        }
        job.transaction_state = TransactionState::Active;
        Ok(())
    }

    // T-043：COMMIT 事务 - active → committing → committed
    pub fn commit_transaction(&self, query_id: &str, generation: u32) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("registry lock");
        let job = guard.get_mut(query_id)
            .ok_or_else(|| format!("查询 {} 不存在", query_id))?;
        if job.generation != generation {
            return Err(format!(
                "generation 不匹配：期望 {}，实际 {}",
                generation, job.generation
            ));
        }
        if !job.transaction_state.can_transition(&TransactionState::Committing) {
            return Err(format!(
                "事务状态 {} 不允许 COMMIT",
                job.transaction_state.as_str()
            ));
        }
        job.transaction_state = TransactionState::Committing;
        Ok(())
    }

    // T-043：标记 COMMIT 完成 - committing → committed
    pub fn mark_committed(&self, query_id: &str) {
        if let Some(job) = self.inner.lock().expect("registry lock").get_mut(query_id) {
            if job.transaction_state == TransactionState::Committing {
                job.transaction_state = TransactionState::Committed;
            }
        }
    }

    // T-043：ROLLBACK 事务 - active → rolling_back
    pub fn rollback_transaction(&self, query_id: &str, generation: u32) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("registry lock");
        let job = guard.get_mut(query_id)
            .ok_or_else(|| format!("查询 {} 不存在", query_id))?;
        if job.generation != generation {
            return Err(format!(
                "generation 不匹配：期望 {}，实际 {}",
                generation, job.generation
            ));
        }
        if !job.transaction_state.can_transition(&TransactionState::RollingBack) {
            return Err(format!(
                "事务状态 {} 不允许 ROLLBACK",
                job.transaction_state.as_str()
            ));
        }
        job.transaction_state = TransactionState::RollingBack;
        Ok(())
    }

    // T-043：标记 ROLLBACK 完成 - rolling_back → rolled_back
    pub fn mark_rolled_back(&self, query_id: &str) {
        if let Some(job) = self.inner.lock().expect("registry lock").get_mut(query_id) {
            if job.transaction_state == TransactionState::RollingBack {
                job.transaction_state = TransactionState::RolledBack;
            }
        }
    }

    // T-043：断线/异常 → unknown
    pub fn mark_transaction_unknown(&self, query_id: &str) {
        if let Some(job) = self.inner.lock().expect("registry lock").get_mut(query_id) {
            job.transaction_state = TransactionState::Unknown;
        }
    }

    // T-043：重置事务状态到 idle
    pub fn reset_transaction_to_idle(&self, query_id: &str) {
        if let Some(job) = self.inner.lock().expect("registry lock").get_mut(query_id) {
            if matches!(
                job.transaction_state,
                TransactionState::Failed | TransactionState::RolledBack
                    | TransactionState::Committed | TransactionState::Unknown
            ) {
                job.transaction_state = TransactionState::Idle;
            }
        }
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
            transaction_state: TransactionState::Idle,
            generation: 1,
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
            transaction_state: TransactionState::Idle,
            generation: 1,
        };
        reg.insert(job);
        assert!(reg.get_state("q2").is_some());
        reg.remove("q2");
        assert!(reg.get_state("q2").is_none());
    }

    // T-043：事务状态机测试
    #[test]
    fn transaction_state_transitions() {
        assert!(TransactionState::Idle.can_transition(&TransactionState::Active));
        assert!(TransactionState::Active.can_transition(&TransactionState::Failed));
        assert!(TransactionState::Active.can_transition(&TransactionState::Committing));
        assert!(TransactionState::Active.can_transition(&TransactionState::RollingBack));
        assert!(TransactionState::Committing.can_transition(&TransactionState::Committed));
        assert!(TransactionState::Committing.can_transition(&TransactionState::Unknown));
        assert!(TransactionState::RollingBack.can_transition(&TransactionState::RolledBack));
        assert!(TransactionState::Failed.can_transition(&TransactionState::Idle));
        assert!(TransactionState::RolledBack.can_transition(&TransactionState::Idle));

        // 非法转换
        assert!(!TransactionState::Idle.can_transition(&TransactionState::Committed));
        assert!(!TransactionState::Active.can_transition(&TransactionState::Committed));
        assert!(!TransactionState::Committed.can_transition(&TransactionState::Active));
    }

    #[test]
    fn transaction_state_any_to_unknown() {
        assert!(TransactionState::Idle.can_transition(&TransactionState::Unknown));
        assert!(TransactionState::Active.can_transition(&TransactionState::Unknown));
        assert!(TransactionState::Committed.can_transition(&TransactionState::Unknown));
    }

    #[test]
    fn transaction_begin_commit_rollback_lifecycle() {
        let reg = QueryRegistry::new();
        let (tx, _rx) = mpsc::channel(1);
        let job = QueryJobInternal {
            query_id: "tx1".into(),
            state: QueryState::Running,
            state_msg: "running".into(),
            cancel_requested: false,
            start_time: Instant::now(),
            batch_tx: tx,
            transaction_state: TransactionState::Idle,
            generation: 1,
        };
        reg.insert(job);

        // BEGIN
        assert!(reg.begin_transaction("tx1", 1).is_ok());
        assert_eq!(reg.get_transaction_state("tx1"), Some(TransactionState::Active));

        // COMMIT
        assert!(reg.commit_transaction("tx1", 1).is_ok());
        assert_eq!(reg.get_transaction_state("tx1"), Some(TransactionState::Committing));

        // COMMIT 完成
        reg.mark_committed("tx1");
        assert_eq!(reg.get_transaction_state("tx1"), Some(TransactionState::Committed));

        // 重置到 idle
        reg.reset_transaction_to_idle("tx1");
        assert_eq!(reg.get_transaction_state("tx1"), Some(TransactionState::Idle));
    }

    #[test]
    fn transaction_generation_mismatch_rejected() {
        let reg = QueryRegistry::new();
        let (tx, _rx) = mpsc::channel(1);
        let job = QueryJobInternal {
            query_id: "tx2".into(),
            state: QueryState::Running,
            state_msg: "running".into(),
            cancel_requested: false,
            start_time: Instant::now(),
            batch_tx: tx,
            transaction_state: TransactionState::Idle,
            generation: 1,
        };
        reg.insert(job);
        let err = reg.begin_transaction("tx2", 2);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("generation"));
    }

    #[test]
    fn transaction_rollback_lifecycle() {
        let reg = QueryRegistry::new();
        let (tx, _rx) = mpsc::channel(1);
        let job = QueryJobInternal {
            query_id: "rb1".into(),
            state: QueryState::Running,
            state_msg: "running".into(),
            cancel_requested: false,
            start_time: Instant::now(),
            batch_tx: tx,
            transaction_state: TransactionState::Idle,
            generation: 1,
        };
        reg.insert(job);

        assert!(reg.begin_transaction("rb1", 1).is_ok());
        assert!(reg.rollback_transaction("rb1", 1).is_ok());
        assert_eq!(reg.get_transaction_state("rb1"), Some(TransactionState::RollingBack));
        reg.mark_rolled_back("rb1");
        assert_eq!(reg.get_transaction_state("rb1"), Some(TransactionState::RolledBack));
        reg.reset_transaction_to_idle("rb1");
        assert_eq!(reg.get_transaction_state("rb1"), Some(TransactionState::Idle));
    }

    #[test]
    fn transaction_unknown_state() {
        let reg = QueryRegistry::new();
        let (tx, _rx) = mpsc::channel(1);
        let job = QueryJobInternal {
            query_id: "unk1".into(),
            state: QueryState::Running,
            state_msg: "running".into(),
            cancel_requested: false,
            start_time: Instant::now(),
            batch_tx: tx,
            transaction_state: TransactionState::Active,
            generation: 1,
        };
        reg.insert(job);
        reg.mark_transaction_unknown("unk1");
        assert_eq!(reg.get_transaction_state("unk1"), Some(TransactionState::Unknown));
    }
}
