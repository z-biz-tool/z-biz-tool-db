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

impl ApprovalError {
    /// 给用户看的原因（**不是** Debug 打印：`Missing("...")` 那层壳对用户没意义）。
    ///
    /// 改之前 lib.rs 用 `format!("审批校验失败: {:?}", e)`，把枚举变体名和引号
    /// 一起糊给用户看；这里把真正的 payload 拿出来。
    pub fn message(&self) -> &str {
        match self {
            ApprovalError::Missing(m)
            | ApprovalError::Expired(m)
            | ApprovalError::SqlMismatch(m)
            | ApprovalError::Consumed(m) => m,
        }
    }
}

impl std::fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ApprovalError {}

pub fn digest_sql(sql: &str) -> String {
    // S0 简化：用 sql 首 64 个字符 + 长度作为摘要；S3 将换成带密钥的 HMAC
    //
    // ⚠ **长度口径必须是「字符数（Unicode 码点）」**，不是字节数、也不是 UTF-16 单元。
    //
    // 2026-10-22 实测修复：这里原来用 `sql.len()`（**字节数**），
    // 而前端 `digestSql` 用 `sql.length`（**UTF-16 单元**）⇒
    // 含中文的 SQL 两端摘要**必然不一样** ⇒ 用户明明批过了，后端却以
    // `SqlMismatch` 拒掉（"审批 sql_digest 与请求不匹配"）。
    // 同一个函数内部也不自洽：head 取的是 `chars()`（码点），len 取的是字节。
    //
    // 统一成码点数后：Rust `chars().count()` ↔ TS `Array.from(sql).length`，
    // 两端一致。两端各有一份黄金向量测试（`security.rs` 的 digest_golden_vectors
    // 与 `tests/digestContract.test.ts`）钉住同一个字面量 ⇒ 改一边必有一边红。
    let chars: Vec<char> = sql.chars().collect();
    let head: String = chars.iter().take(64).collect();
    format!("len={}|head={}", chars.len(), head)
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
        let err = evaluate(
            None,
            c.kind,
            c.safety,
            "INSERT INTO t VALUES (1)",
            now(),
            "dev",
        )
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
        let err =
            evaluate(Some(&grant), c.kind, c.safety, sql, now(), "dev").expect_err("过期应拒");
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
        assert!(
            matches!(err, ApprovalError::SqlMismatch(_)),
            "got {:?}",
            err
        );
    }

    /// 摘要黄金向量（前端 `tests/digestContract.test.ts` 钉的是**同一批字面量**）。
    ///
    /// 为什么要有这组：长度口径曾经是「Rust 字节数 / JS UTF-16 单元」，
    /// 含中文或 emoji 的 SQL 两端必然算出不同的摘要 ⇒ 合法审批被后端误拒。
    /// 这组向量里必须有**非 ASCII** 与**代理对（emoji）**——
    /// 只有 ASCII 时三种口径结果一样，测不出问题。
    #[test]
    fn digest_golden_vectors() {
        assert_eq!(digest_sql("SELECT 1"), "len=8|head=SELECT 1");
        assert_eq!(digest_sql("SELECT '中'"), "len=10|head=SELECT '中'");
        assert_eq!(
            digest_sql("INSERT INTO t VALUES ('😀')"),
            "len=26|head=INSERT INTO t VALUES ('😀')"
        );
        assert_eq!(
            digest_sql("CSV_IMPORT table=users rows=3"),
            "len=29|head=CSV_IMPORT table=users rows=3"
        );
        // 超过 64 个字符时 head 截断（取**字符**不是字节）
        let long = "a".repeat(70);
        assert_eq!(digest_sql(&long), format!("len=70|head={}", "a".repeat(64)));
    }

    /// 非 ASCII 的 SQL：摘要必须与「按码点数」一致（不是字节数）
    #[test]
    fn digest_counts_code_points_not_bytes() {
        // "中" 占 3 字节、1 个码点；"😀" 占 4 字节、1 个码点、2 个 UTF-16 单元
        assert!(!digest_sql("中").contains("len=3"), "不能按字节数算");
        assert!(digest_sql("中").starts_with("len=1|"), "按码点数算");
        assert!(digest_sql("😀").starts_with("len=1|"), "emoji 也是一个码点");
    }
}
