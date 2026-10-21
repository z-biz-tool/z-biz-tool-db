// T-019：v2 IPC 契约 —— **已接线**到 metadata_list_v2 / query_status_v2 / query_cancel_v2。
//
// 统一包络：成功与失败走同一个返回类型，调用方按 `kind` 分支。
// 错误码是**机器可读**的（前端 src/ipc/errors.ts 优先认这个前缀），
// safe_message 是给人看的（不得含凭据/连接串）。
//
// 错误码清单与前端 KNOWN_PATTERNS 一一对应；改这里要同步改那边。

use serde::{Deserialize, Serialize};

/// 只读通道拒绝写/危险语句
pub const CODE_READONLY_BLOCKED: &str = "READONLY_BLOCKED";
/// 审批缺失/过期/摘要不符/已用
pub const CODE_APPROVAL_DENIED: &str = "APPROVAL_DENIED";
/// 连接/会话代次过期
pub const CODE_STALE_GENERATION: &str = "STALE_GENERATION";
/// 端口字段非法
pub const CODE_INVALID_PORT: &str = "INVALID_PORT";
/// 静态分类器拒绝
pub const CODE_SQL_CLASSIFY: &str = "SQL_CLASSIFY";
/// 连接建立失败（网络/凭据/握手/TLS）
pub const CODE_CONNECTION_FAILED: &str = "CONNECTION_FAILED";
/// 兜底
pub const CODE_UNKNOWN: &str = "UNKNOWN";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub stage: String,
    pub retryable: bool,
    pub query_id: Option<String>,
    pub transaction_state: Option<String>,
    pub safe_message: String,
}

impl ApiError {
    pub fn new(code: &str, stage: &str, safe_message: &str) -> Self {
        Self {
            code: code.to_string(),
            stage: stage.to_string(),
            retryable: false,
            query_id: None,
            transaction_state: None,
            safe_message: safe_message.to_string(),
        }
    }

    pub fn readonly_blocked(stmt_kind: &str, safety: &str) -> Self {
        Self::new(
            CODE_READONLY_BLOCKED,
            "execute_query",
            &format!("只读模式拒绝 {:?} (safety={:?})", stmt_kind, safety),
        )
    }

    pub fn approval_denied(reason: &str) -> Self {
        Self::new(CODE_APPROVAL_DENIED, "execute_query", reason)
    }

    pub fn stale_generation(s: &str) -> Self {
        Self::new(CODE_STALE_GENERATION, "execute_query", s)
    }

    pub fn invalid_port(s: &str) -> Self {
        Self::new(CODE_INVALID_PORT, "connection_open", s)
    }

    pub fn sql_classify(s: &str) -> Self {
        Self::new(CODE_SQL_CLASSIFY, "execute_query", s)
    }

    /// 归不进任何已知类别的错误（SQL 语法错、驱动返回的意外错误…）。
    ///
    /// 归成 UNKNOWN 而不是让前端去猜：前端猜不出来就会显示成"连接问题"，
    /// 把 SQL 语法错说成网络问题 ⇒ 用户会去查网络，白查。
    pub fn unknown(stage: &str, s: &str) -> Self {
        let mut e = Self::new(CODE_UNKNOWN, stage, s);
        e.retryable = true;
        e
    }

    pub fn connection_failed(s: &str) -> Self {
        let mut e = Self::new(CODE_CONNECTION_FAILED, "connect", s);
        // 网络/握手问题重试有意义；凭据问题没有，但前端按大类给 retryable=true，
        // 真正要不要重试由用户决定，这里不替用户决定。
        e.retryable = true;
        e
    }

    /// 关联到具体的一次查询（取消/状态接口用）
    pub fn with_query_id(mut self, query_id: &str) -> Self {
        self.query_id = Some(query_id.to_string());
        self
    }

    /// 关联到事务状态（事务类失败用）
    pub fn with_transaction_state(mut self, state: &str) -> Self {
        self.transaction_state = Some(state.to_string());
        self
    }

    /// 线上传输形态：`CODE: message`。
    ///
    /// 前端 errors.ts 先按这个前缀取机器可读 code，取不到再退回中文正则匹配。
    /// 中文部分保持原样 ⇒ 老前端不受影响。
    pub fn to_wire(&self) -> String {
        format!("{}: {}", self.code, self.safe_message)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_wire())
    }
}

impl std::error::Error for ApiError {}

/// v2 后端响应统一包络
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum V2Envelope<T> {
    Ok { data: T, query_id: String },
    Err(ApiError),
}

impl<T> V2Envelope<T> {
    pub fn ok(data: T, query_id: &str) -> Self {
        V2Envelope::Ok {
            data,
            query_id: query_id.to_string(),
        }
    }

    pub fn err(e: ApiError) -> Self {
        V2Envelope::Err(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_error_carries_kind() {
        let e = ApiError::readonly_blocked("Dml", "Write");
        assert_eq!(e.code, "READONLY_BLOCKED");
        assert!(!e.retryable);
    }

    #[test]
    fn approval_error_when_missing() {
        let e = ApiError::approval_denied("missing grant");
        assert_eq!(e.code, "APPROVAL_DENIED");
    }

    #[test]
    fn envelope_serializes_ok_branch() {
        let env: V2Envelope<i32> = V2Envelope::Ok {
            data: 42,
            query_id: "q-1".into(),
        };
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains("\"kind\":\"ok\""));
        assert!(s.contains("\"data\":42"));
    }

    #[test]
    fn envelope_serializes_err_branch() {
        let env: V2Envelope<i32> = V2Envelope::Err(ApiError::new("X", "y", "z"));
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains("\"kind\":\"err\""));
        assert!(s.contains("\"code\":\"X\""));
    }

    /// 线上形态必须是 `CODE: message`，且中文正文保留（老前端靠正则认它）
    #[test]
    fn wire_form_is_code_prefixed_and_keeps_chinese_body() {
        let e = ApiError::readonly_blocked("Dml", "Write");
        let w = e.to_wire();
        assert!(w.starts_with("READONLY_BLOCKED: "), "实际: {}", w);
        assert!(w.contains("只读模式拒绝"), "实际: {}", w);
    }

    #[test]
    fn envelope_has_ok_and_err_branches() {
        let env: V2Envelope<i32> = V2Envelope::ok(7, "q-9");
        match env {
            V2Envelope::Ok { data, query_id } => {
                assert_eq!(data, 7);
                assert_eq!(query_id, "q-9");
            }
            V2Envelope::Err(e) => panic!("不该是 err 分支: {}", e),
        }

        let env: V2Envelope<i32> = V2Envelope::err(ApiError::stale_generation("代次过期"));
        match env {
            V2Envelope::Ok { .. } => panic!("不该是 ok 分支"),
            V2Envelope::Err(e) => assert_eq!(e.code, "STALE_GENERATION"),
        }
    }
}
