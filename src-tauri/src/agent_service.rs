// T-048: 统一 Agent 适配器
//
// 当前有三套不一致路径（T-048 审计发现）：
//   (1) AI HTTP: call_ai_service（config 直传）
//   (2) 本地 Agent: call_agent_service（localhost:8787）
//   (3) 前端 zustand AgentManager（示例数据）
//
// 本模块统一为一条后端路径：agent_assist_v2
// - Agent 不接数据库执行句柄（T-004 / A08）
// - 建议仅为草稿，用户手动执行走常规授权（T-049 / A09）
// - 上下文受最小必要原则约束，样本脱敏由前端预处理

use serde::{Serialize, Deserialize};
use crate::db::ipc::ApiError;
use crate::db::sql_classify::{classify, SafetyClass};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentIntent {
    Suggest,     // 自然语言 → SQL
    Optimize,    // SQL 优化
    Analyze,     // 结果分析
    Diagnose,    // 错误诊断
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub intent: AgentIntent,
    pub sql: Option<String>,
    pub natural_language: Option<String>,
    pub result_data: Option<Vec<serde_json::Value>>,
    pub error_message: Option<String>,
    pub connection_info: Option<AgentConnectionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConnectionInfo {
    pub connection_id: String,
    pub database_type: String,
    pub database_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub content: String,
    pub suggested_sql: Option<String>,
    pub is_draft: bool,
}

/// T-048/T-049: 统一 Agent 入口
/// 1. 不获取数据库执行句柄
/// 2. Analyze intent 必须显式传 result_data
/// 3. 服务不可达时返回明确错误
pub async fn agent_assist_v2(req: AgentRequest) -> Result<AgentResponse, ApiError> {
    match &req.intent {
        AgentIntent::Suggest => {
            let sql = req.natural_language.as_deref().ok_or_else(|| {
                ApiError::new("AGENT_MISSING_INPUT", "agent_assist_v2", "suggestion requires natural_language field")
            })?;
            // 目前 S0 后端 Agent 服务不可用
            Err(ApiError::new(
                "AGENT_SERVICE_UNAVAILABLE",
                "agent_assist_v2",
                &format!("Agent suggestion for '{}' - service unavailable; returning not_configured", sql.chars().take(20).collect::<String>()),
            ))
        }
        AgentIntent::Optimize => {
            let sql = req.sql.as_deref().ok_or_else(|| {
                ApiError::new("AGENT_MISSING_SQL", "agent_assist_v2", "optimize requires sql field")
            })?;
            // SQL 安全检查：不允许在优化结果里执行破坏性操作
            let classification = classify(sql);
            if classification.safety == SafetyClass::Write || classification.safety == SafetyClass::Unknown {
                return Err(ApiError::new(
                    "AGENT_UNSAFE_SQL",
                    "agent_assist_v2",
                    "optimize target SQL contains write or unknown statements; refusing suggestion",
                ));
            }
            // 服务不可用
            Err(ApiError::new(
                "AGENT_SERVICE_UNAVAILABLE",
                "agent_assist_v2",
                &format!("SQL optimize for first {} chars - service unavailable", sql.chars().take(20).collect::<String>()),
            ))
        }
        AgentIntent::Analyze => {
            let results = req.result_data.as_ref().ok_or_else(|| {
                ApiError::new("AGENT_MISSING_RESULTS", "agent_assist_v2", "analyze requires explicit result_data; no implicit SQL execution allowed")
            })?;
            let sql = req.sql.as_deref().unwrap_or("<no SQL>");
            // 服务不可用
            Err(ApiError::new(
                "AGENT_SERVICE_UNAVAILABLE",
                "agent_assist_v2",
                &format!("Analyze {} rows for first {} chars of SQL - service unavailable",
                    results.len(), sql.chars().take(20).collect::<String>()),
            ))
        }
        AgentIntent::Diagnose => {
            let error = req.error_message.as_deref().unwrap_or("<no error>");
            let sql = req.sql.as_deref().unwrap_or("<no SQL>");
            Err(ApiError::new(
                "AGENT_SERVICE_UNAVAILABLE",
                "agent_assist_v2",
                &format!("Diagnose first {} chars of error - service unavailable", error.chars().take(20).collect::<String>()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_async<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(f)
    }

    #[test]
    fn agent_v2_returns_unavailable_when_service_down() {
        let req = AgentRequest {
            intent: AgentIntent::Suggest,
            sql: None,
            natural_language: Some("show all users".into()),
            result_data: None,
            error_message: None,
            connection_info: None,
        };
        let result = run_async(agent_assist_v2(req));
        assert!(result.is_err());
        let e = result.unwrap_err();
        assert_eq!(e.code, "AGENT_SERVICE_UNAVAILABLE");
        assert!(e.safe_message.contains("unavailable"));
    }

    #[test]
    fn agent_v2_analyze_requires_explicit_results() {
        let req = AgentRequest {
            intent: AgentIntent::Analyze,
            sql: Some("SELECT * FROM t".into()),
            natural_language: None,
            result_data: None, // 不传结果数据
            error_message: None,
            connection_info: None,
        };
        let result = run_async(agent_assist_v2(req));
        assert!(result.is_err());
        let e = result.unwrap_err();
        assert_eq!(e.code, "AGENT_MISSING_RESULTS");
    }

    #[test]
    fn agent_v2_optimize_rejects_write_sql() {
        let req = AgentRequest {
            intent: AgentIntent::Optimize,
            sql: Some("DELETE FROM users".into()),
            natural_language: None,
            result_data: None,
            error_message: None,
            connection_info: None,
        };
        let result = run_async(agent_assist_v2(req));
        assert!(result.is_err());
        let e = result.unwrap_err();
        assert_eq!(e.code, "AGENT_UNSAFE_SQL");
    }
}
