// T-013：查询取消协议
//
// 取消 ≠ kill connection：S0 只记录 cancel_requested 状态并暴露 cmd，
// 真正的数据库级取消（PG pg_cancel_backend / MySQL KILL QUERY / SQLite interrupt）
// 由后续版本接入驱动层。
//
// 口径：
// - cancel_query 接受 queryId，返回 accepted/cancelState 状态；
// - 状态字段：`accepted`（已收到但未确认）/ `pending`（2s 未确认）/ `unknown`（无法保证）。
// - 不允许前端伪造 queryId 触发任意取消：queryId 必须能匹配到本进程缓存中的 in-flight 单子。
//
// **2026-10-22 修正**：改之前这里只有 `cancel()` 一个方法真正被用过，
// 而**从来没有 register 过任何 in-flight 单子** ⇒ 任何取消请求都只能拿到 `NotFound`
// （也就是说"取消协议"其实是不工作的）。同时注册表是 `query_cancel_v2` 里的
// 一个函数内 `static`，`query_status_v2` 又完全查不到它 ⇒ 状态与取消对不上。
// 现在：注册表提成全局单例；查询开始时 register、结束时 finish；
// 状态查询能真的查到这一单，并给出取消状态。

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

/// 取消请求受理后，多久算"还没确认"（毫秒）。
pub const CANCEL_CONFIRM_TIMEOUT_MS: i64 = 2_000;

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

/// 一条在途查询。字段全是**可对外的**：状态查询要能把"哪条 SQL、哪条连接、什么时候开始"
/// 报给界面，否则取消只能靠一个孤零零的 queryId。
#[derive(Debug, Clone, serde::Serialize)]
pub struct InFlight {
    pub query_id: String,
    /// 所属连接：关连接时要把这条连接上的在途单子一并作废。
    pub connection_id: String,
    pub sql_digest: String,
    /// 起始时间（Unix 秒）
    pub started_at: i64,
    pub cancel_requested: bool,
    /// 取消请求被受理的时间（Unix 毫秒）；未请求取消时为 None。
    pub cancel_requested_at: Option<i64>,
}

/// 状态查询的返回：既报在途信息，也报取消状态。
///
/// `cancel_state` 为 None = **没请求过取消**（不是"取消失败"）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryStatus {
    pub query_id: String,
    pub connection_id: String,
    pub sql_digest: String,
    pub started_at: i64,
    pub cancel_requested: bool,
    pub cancel_state: Option<CancelState>,
    pub message: String,
}

#[derive(Default)]
pub struct InFlightRegistry {
    inner: StdMutex<HashMap<String, InFlight>>,
    /// 已被作废、且**取消结果无法确认**的单子（连接被关掉 / 池被回收时产生）。
    /// 记下来是因为：连接都没了 ⇒ 我们无从知道服务端停没停 ⇒ 只能报 `Unknown`，
    /// 而不是把"查无此单"说成"没取消过"。
    invalidated: StdMutex<HashMap<String, CancelState>>,
}

impl InFlightRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一条在途查询。**查询一开始就调** —— 否则取消永远查无此单。
    pub fn register(&self, connection_id: &str, query_id: String, sql_digest: String) -> InFlight {
        let entry = InFlight {
            query_id: query_id.clone(),
            connection_id: connection_id.to_string(),
            sql_digest,
            started_at: chrono::Utc::now().timestamp(),
            cancel_requested: false,
            cancel_requested_at: None,
        };
        let mut guard = self.inner.lock().expect("poisoned");
        guard.insert(query_id, entry.clone());
        entry
    }

    /// 请求取消。查无此单 ⇒ `NotFound`（不伪造成功）。
    ///
    /// 但如果这一单是**被作废**的（连接已关），那不是"没这单"，
    /// 而是"这单的结果无法确认" ⇒ 报 `Unknown`（连接应当被隔离）。
    pub fn cancel(&self, query_id: &str) -> CancelResponse {
        let mut guard = self.inner.lock().expect("poisoned");
        if let Some(state) = self
            .invalidated
            .lock()
            .expect("poisoned")
            .get(query_id)
            .copied()
        {
            return CancelResponse {
                query_id: query_id.to_string(),
                state,
                message: "该查询所在连接已被关闭，取消结果无法确认；请隔离该连接后重连".to_string(),
            };
        }
        match guard.get_mut(query_id) {
            None => CancelResponse {
                query_id: query_id.to_string(),
                state: CancelState::NotFound,
                message: format!("未找到 in-flight 查询：{}", query_id),
            },
            Some(entry) => {
                entry.cancel_requested = true;
                entry.cancel_requested_at = Some(chrono::Utc::now().timestamp_millis());
                CancelResponse {
                    query_id: query_id.to_string(),
                    state: CancelState::Accepted,
                    message: "已接受取消请求；等待驱动确认".to_string(),
                }
            }
        }
    }

    /// 查一单的状态。查无此单 ⇒ None。
    pub fn status(&self, query_id: &str) -> Option<QueryStatus> {
        let guard = self.inner.lock().expect("poisoned");
        let Some(entry) = guard.get(query_id) else {
            // 不在途了：看看是不是被作废的（那时取消结果是 Unknown，不是"没这单"）
            let inv = self.invalidated.lock().expect("poisoned");
            let state = inv.get(query_id).copied()?;
            return Some(QueryStatus {
                query_id: query_id.to_string(),
                connection_id: String::new(),
                sql_digest: String::new(),
                started_at: 0,
                cancel_requested: state != CancelState::NotFound,
                cancel_state: Some(state),
                message: "该查询所在连接已被关闭，状态无法确认".to_string(),
            });
        };
        let cancel_state = if !entry.cancel_requested {
            None
        } else {
            let waited = entry
                .cancel_requested_at
                .map(|at| chrono::Utc::now().timestamp_millis() - at)
                .unwrap_or(0);
            Some(if waited >= CANCEL_CONFIRM_TIMEOUT_MS {
                CancelState::Pending
            } else {
                CancelState::Accepted
            })
        };
        let message = match cancel_state {
            None => "查询在途；尚未请求取消".to_string(),
            Some(CancelState::Accepted) => "取消请求已受理，等待驱动确认".to_string(),
            Some(CancelState::Pending) => format!(
                "取消请求已受理超过 {}ms，仍未确认",
                CANCEL_CONFIRM_TIMEOUT_MS
            ),
            Some(_) => "取消状态未知".to_string(),
        };
        Some(QueryStatus {
            query_id: entry.query_id.clone(),
            connection_id: entry.connection_id.clone(),
            sql_digest: entry.sql_digest.clone(),
            started_at: entry.started_at,
            cancel_requested: entry.cancel_requested,
            cancel_state,
            message,
        })
    }

    /// 查询结束（正常结束或被取消）⇒ 从在途表里摘掉。
    ///
    /// 返回值：若这一单**曾经请求过取消**，返回 `Confirmed`（取消已生效）；
    /// 否则返回 None（它是正常跑完的，与取消无关）。
    pub fn finish(&self, query_id: &str) -> Option<CancelState> {
        let mut guard = self.inner.lock().expect("poisoned");
        let entry = guard.remove(query_id)?;
        if entry.cancel_requested {
            Some(CancelState::Confirmed)
        } else {
            None
        }
    }

    /// 作废**某条连接**上的全部在途单子（关连接时用），并把它们交还。
    ///
    /// 返回的单子里 `cancel_requested == true` 的那些 ⇒ 它们的取消**无法确认**
    /// （连接都没了）⇒ 调用方应报 `Unknown` 并隔离该连接。
    pub fn drain(&self, connection_id: &str) -> Vec<InFlight> {
        let mut guard = self.inner.lock().expect("poisoned");
        let v: Vec<InFlight> = guard
            .values()
            .filter(|e| e.connection_id == connection_id)
            .cloned()
            .collect();
        guard.retain(|_, e| e.connection_id != connection_id);
        // 池没了 ⇒ 服务端停没停我们不知道 ⇒ 曾请求过取消的那些记成 Unknown。
        let mut inv = self.invalidated.lock().expect("poisoned");
        for e in &v {
            if e.cancel_requested {
                inv.insert(e.query_id.clone(), CancelState::Unknown);
            }
        }
        v
    }

    /// 已请求取消、但还没确认的条数。
    pub fn pending_count(&self) -> usize {
        let guard = self.inner.lock().expect("poisoned");
        guard.values().filter(|e| e.cancel_requested).count()
    }

    /// 在途总数。
    pub fn in_flight_count(&self) -> usize {
        self.inner.lock().expect("poisoned").len()
    }
}

static REGISTRY: std::sync::OnceLock<InFlightRegistry> = std::sync::OnceLock::new();

/// 全局唯一的在途注册表。
///
/// 必须是**唯一**的：取消命令与状态命令查的是同一张表，
/// 否则「取消说受理了、状态说查无此单」这种自相矛盾必然发生。
pub fn in_flight() -> &'static InFlightRegistry {
    REGISTRY.get_or_init(InFlightRegistry::new)
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
        r.register("c1", "q1".into(), "SELECT 1".into());
        let resp = r.cancel("q1");
        assert!(matches!(resp.state, CancelState::Accepted));
        assert_eq!(r.pending_count(), 1);
    }

    #[test]
    fn drain_clears_in_flight() {
        let r = InFlightRegistry::new();
        r.register("c1", "q1".into(), "SELECT 1".into());
        r.register("c1", "q2".into(), "SELECT 2".into());
        let drained = r.drain("c1");
        assert_eq!(drained.len(), 2);
        let resp = r.cancel("q1");
        assert!(matches!(resp.state, CancelState::NotFound));
    }

    /// 关闭一条连接时，**别的连接**的在途单子必须留着。
    #[test]
    fn drain_only_touches_the_named_connection() {
        let r = InFlightRegistry::new();
        r.register("c1", "q1".into(), "SELECT 1".into());
        r.register("c2", "q2".into(), "SELECT 2".into());
        let drained = r.drain("c1");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].query_id, "q1");
        assert!(r.status("q2").is_some(), "c2 的在途单子必须留着");
    }

    /// 查询在取消请求之后结束 ⇒ 取消**已生效**（Confirmed）。
    #[test]
    fn finish_after_cancel_reports_confirmed() {
        let r = InFlightRegistry::new();
        r.register("c1", "q1".into(), "SELECT 1".into());
        assert!(r.finish("q1").is_none(), "没请求过取消时不该报取消状态");
        assert!(r.status("q1").is_none(), "结束后不在在途表里");

        r.register("c1", "q2".into(), "SELECT 2".into());
        r.cancel("q2");
        assert_eq!(r.finish("q2"), Some(CancelState::Confirmed));
    }

    /// 状态查询要能报出这一单是"哪条连接、哪条 SQL、什么时候开始"。
    #[test]
    fn status_reports_in_flight_details() {
        let r = InFlightRegistry::new();
        r.register("c1", "q1".into(), "SELECT 1".into());
        let s = r.status("q1").expect("在途");
        assert_eq!(s.connection_id, "c1");
        assert_eq!(s.sql_digest, "SELECT 1");
        assert!(s.started_at > 0);
        assert!(!s.cancel_requested);
        assert_eq!(s.cancel_state, None, "没请求过取消时 cancel_state 是 None");
    }

    /// 请求取消之后、还没结束 ⇒ 状态是 Accepted（不是 Pending）。
    #[test]
    fn status_after_cancel_is_accepted_until_timeout() {
        let r = InFlightRegistry::new();
        r.register("c1", "q1".into(), "SELECT 1".into());
        r.cancel("q1");
        let s = r.status("q1").expect("在途");
        assert!(s.cancel_requested);
        assert_eq!(s.cancel_state, Some(CancelState::Accepted));
    }
}
