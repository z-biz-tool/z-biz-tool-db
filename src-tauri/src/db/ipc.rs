// T-019：v2 IPC 契约（拟定）
//
// 用于 S1 后端 Session Actor 与前端之间的统一协议，
// 不替换 S0 命令签名；新字段用 #[serde(default)] 兼容旧调用。

use serde::{Deserialize, Serialize};

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

    pub fn authz(safe_message: &str) -> Self {
        Self::new("AUTHZ_DENIED", "authz", safe_message)
    }
    pub fn readonly_blocked(stmt_kind: &str, safety: &str) -> Self {
        Self::new(
            "READONLY_BLOCKED",
            "execute_query",
            &format!("只读模式拒绝 {:?} (safety={:?})", stmt_kind, safety),
        )
    }
    pub fn approval_denied(reason: &str) -> Self {
        Self::new("APPROVAL_DENIED", "execute_query", reason)
    }
    pub fn stale_generation(s: &str) -> Self {
        Self::new("STALE_GENERATION", "execute_query", s)
    }
    pub fn invalid_port(s: &str) -> Self {
        Self::new("INVALID_PORT", "connection_open", s)
    }
}

/// v2 后端响应统一包络
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum V2Envelope<T> {
    Ok { data: T, query_id: String },
    Err(ApiError),
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
}
