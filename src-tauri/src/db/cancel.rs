// T-013：查询取消协议骨架
//
// 取消 ≠ kill connection：S0 仅记录 cancel_requested 状态并暴露 cmd，
// 真正的数据库级取消（PG pg_cancel_backend / MySQL KILL QUERY / SQLite interrupt）
// 由后续版本接入驱动层。
//
// S0 保证：
// - cancel_query 接受 queryId，返回 accepted/cancelState 状态；
// - 状态字段：`accepted`（已收到但未确认）/ `pending`（2s 未确认）/ `unknown`（无法保证）。
// - 不允许前端伪造 queryId 触发任意取消：queryId 必须能匹配到本进程缓存中的 in-flight 单子。

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CancelState {
    /// 未找到匹配 in-flight 查询
    NotFound,
    /// 取消请求被服务端接受，等待数据库/驱动确认
    Accepted,
    /// 取消已生效，连接/事务已清理
    Confirmed,
    /// 2 秒内未确认；UI 可显示"正在请求取消"
    Pending,
    /// 无法确认服务端是否停止；连接应隔离
    Unknown,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CancelResponse {
    pub query_id: String,
    pub state: CancelState,
    pub message: String,
}

#[derive(Default)]
pub struct InFlightRegistry {
    inner: StdMutex<HashMap<String, InFlight>>,
    seq: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct InFlight {
    pub query_id: String,
    pub sql_digest: String,
    pub started_at: i64,
    pub cancel_requested: bool,
}

impl InFlightRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, query_id: String, sql_digest: String) -> InFlight {
        let entry = InFlight {
            query_id: query_id.clone(),
            sql_digest,
            started_at: chrono::Utc::now().timestamp(),
            cancel_requested: false,
        };
        let mut guard = self.inner.lock().expect("poisoned");
        guard.insert(query_id, entry.clone());
        entry
    }

    pub fn cancel(&self, query_id: &str) -> CancelResponse {
        let mut guard = self.inner.lock().expect("poisoned");
        match guard.get_mut(query_id) {
            None => CancelResponse {
                query_id: query_id.to_string(),
                state: CancelState::NotFound,
                message: format!("未找到 in-flight 查询：{}", query_id),
            },
            Some(entry) => {
                entry.cancel_requested = true;
                CancelResponse {
                    query_id: query_id.to_string(),
                    state: CancelState::Accepted,
                    message: "已接受取消请求；等待驱动确认".to_string(),
                }
            }
        }
    }

    pub fn drain(&self) -> Vec<InFlight> {
        let mut guard = self.inner.lock().expect("poisoned");
        let v: Vec<InFlight> = guard.values().cloned().collect();
        guard.clear();
        v
    }

    pub fn pending_count(&self) -> usize {
        let guard = self.inner.lock().expect("poisoned");
        guard.values().filter(|e| e.cancel_requested).count()
    }

    pub fn next_query_id(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("q-{}-{}", chrono::Utc::now().timestamp_millis(), n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_unknown_returns_not_found() {
        let r = InFlightRegistry::new();
        let resp = r.cancel("q-nonexistent");
        assert!(matches!(resp.state, CancelState::NotFound));
    }

    #[test]
    fn cancel_known_returns_accepted() {
        let r = InFlightRegistry::new();
        r.register("q1".into(), "SELECT 1".into());
        let resp = r.cancel("q1");
        assert!(matches!(resp.state, CancelState::Accepted));
        assert_eq!(r.pending_count(), 1);
    }

    #[test]
    fn drain_clears_in_flight() {
        let r = InFlightRegistry::new();
        r.register("q1".into(), "SELECT 1".into());
        r.register("q2".into(), "SELECT 2".into());
        let drained = r.drain();
        assert_eq!(drained.len(), 2);
        let resp = r.cancel("q1");
        assert!(matches!(resp.state, CancelState::NotFound));
    }

    #[test]
    fn next_query_id_unique() {
        let r = InFlightRegistry::new();
        let a = r.next_query_id();
        let b = r.next_query_id();
        assert_ne!(a, b);
    }
}
