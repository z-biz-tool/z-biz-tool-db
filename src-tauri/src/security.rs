// T-024：审批门禁骨架
//
// 写入/危险语句需要带 approval_id；S0 仅提供结构与本地逻辑，
// 不签名、不跨进程；S3 由前端 ApprovalDialog 调用 approve_write 注入。

use serde::{Deserialize, Serialize};

use crate::db::sql_classify::{SafetyClass, StatementKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalGrant {
    pub approval_id: String,
    pub sql_digest: String,
    pub environment: String,
    pub issued_at: i64,
    pub expires_at: i64,
    #[serde(default)]
    pub consumed: bool,
}

#[derive(Debug)]
pub enum ApprovalError {
    Missing(String),
    Expired(String),
    SqlMismatch(String),
    Consumed(String),
}

pub fn digest_sql(sql: &str) -> String {
    // S0 简化：用 sql 首 64 字节 + 长度作为摘要；S3 将换成带密钥的 HMAC
    let head: String = sql.chars().take(64).collect();
    format!("len={}|head={}", sql.len(), head)
}

/// 评估一次执行请求是否被授权
pub fn evaluate(
    grant: Option<&ApprovalGrant>,
    kind: StatementKind,
    safety: SafetyClass,
    sql: &str,
    now_ts: i64,
    environment: &str,
) -> Result<(), ApprovalError> {
    if safety == SafetyClass::ReadOnlySafe && kind == StatementKind::Query {
        return Ok(());
    }
    let g = grant.ok_or_else(|| {
        ApprovalError::Missing(format!(
            "执行 {:?} (safety={:?}) 需要显式审批（环境={}）",
            kind, safety, environment
        ))
    })?;
    if g.consumed {
        return Err(ApprovalError::Consumed(format!(
            "审批 {} 已使用，不可重放",
            g.approval_id
        )));
    }
    if now_ts > g.expires_at {
        return Err(ApprovalError::Expired(format!(
            "审批 {} 已过期（now={}, expires={}）",
            g.approval_id, now_ts, g.expires_at
        )));
    }
    if g.sql_digest != digest_sql(sql) {
        return Err(ApprovalError::SqlMismatch(format!(
            "审批 {}.sql_digest 与请求不匹配",
            g.approval_id
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql_classify::classify;

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn make_grant(sql: &str, ttl_secs: i64) -> ApprovalGrant {
        let ts = now();
        ApprovalGrant {
            approval_id: uuid::Uuid::new_v4().to_string(),
            sql_digest: digest_sql(sql),
            environment: "dev".to_string(),
            issued_at: ts,
            expires_at: ts + ttl_secs,
            consumed: false,
        }
    }

    #[test]
    fn select_without_grant_ok() {
        let c = classify("SELECT 1");
        assert!(evaluate(None, c.kind, c.safety, "SELECT 1", now(), "dev").is_ok());
    }

    #[test]
    fn insert_without_grant_rejected() {
        let c = classify("INSERT INTO t VALUES (1)");
        let err = evaluate(None, c.kind, c.safety, "INSERT INTO t VALUES (1)", now(), "dev")
            .expect_err("写入需审批");
        assert!(matches!(err, ApprovalError::Missing(_)), "got {:?}", err);
    }

    #[test]
    fn insert_with_grant_accepted() {
        let sql = "INSERT INTO t VALUES (1)";
        let grant = make_grant(sql, 60);
        let c = classify(sql);
        assert!(
            evaluate(Some(&grant), c.kind, c.safety, sql, now(), "dev").is_ok(),
            "含有效审批应通过"
        );
    }

    #[test]
    fn insert_with_expired_grant_rejected() {
        let sql = "INSERT INTO t VALUES (1)";
        let mut grant = make_grant(sql, 60);
        grant.expires_at = now() - 10;
        let c = classify(sql);
        let err = evaluate(Some(&grant), c.kind, c.safety, sql, now(), "dev")
            .expect_err("过期应拒");
        assert!(matches!(err, ApprovalError::Expired(_)), "got {:?}", err);
    }

    #[test]
    fn sql_mismatch_rejected() {
        let grant = make_grant("INSERT INTO t VALUES (1)", 60);
        let c = classify("INSERT INTO t VALUES (999)");
        let err = evaluate(
            Some(&grant),
            c.kind,
            c.safety,
            "INSERT INTO t VALUES (999)",
            now(),
            "dev",
        )
        .expect_err("摘要不匹配应拒");
        assert!(matches!(err, ApprovalError::SqlMismatch(_)), "got {:?}", err);
    }
}
