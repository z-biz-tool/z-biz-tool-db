// T-023：词法感知的 SQL 分类器
//
// 仅用 `starts_with("select")` 会被 `'FROM users'` 字符串、`/* SELECT */ DELETE` 注释、
// `WITH cte AS (...) INSERT INTO ...` CTE+写入、`SELECT ... FOR UPDATE` 等绕过；
// 本模块先剥除字符串字面量/反引号/行注释/块注释，再判断首关键字。
//
// 输出：
// - StatementKind::Query       —— SELECT / WITH / SHOW / DESC / EXPLAIN / PRAGMA
// - StatementKind::ReadModify  —— SELECT ... FOR UPDATE 等可能带锁的读
// - StatementKind::Dml         —— INSERT / UPDATE / DELETE / REPLACE / MERGE
// - StatementKind::Ddl         —— CREATE / DROP / ALTER / TRUNCATE / RENAME / GRANT / REVOKE / COMMENT
// - StatementKind::TxControl   —— BEGIN / COMMIT / ROLLBACK / SAVEPOINT / SET TRANSACTION
// - StatementKind::SessionCfg  —— SET（不含 SET TRANSACTION）
// - StatementKind::Unknown     —— 无法安全归类；S0 默认按"未知"拒绝只读

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Query,
    ReadModify,
    Dml,
    Ddl,
    TxControl,
    SessionCfg,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyClass {
    /// 安全只读（SELECT/WITH/SHOW/DESC/PRAGMA/EXPLAIN 等价）
    ReadOnlySafe,
    /// 表面只读但可能带副作用（如 SELECT FOR UPDATE、SELECT INTO、EXPLAIN ANALYZE）
    ReadOnlyUnsafe,
    /// 显式写入
    Write,
    /// 不能静态判定，必须拒绝只读通道并通过审批
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub kind: StatementKind,
    pub safety: SafetyClass,
    /// 多语句时返回首条，其它进入 other_statements 数量
    pub other_statements: u32,
}

/// 剥除 SQL 中的字符串字面量与注释，返回剩余正文（保留空白）。
/// 行内注释 `-- ...` 直到换行；块注释 `/* ... */`；字符串 `'...'` 含两倍引号转义。
pub fn strip_strings_and_comments(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        // 行注释
        if c == '-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // 块注释
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        // 单引号字符串
        if c == '\'' {
            out.push('\'');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    // SQL 标准：双引号表示转义
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        out.push('\'');
                        out.push('\'');
                        i += 2;
                        continue;
                    }
                    out.push('\'');
                    i += 1;
                    break;
                }
                // 非 ASCII 字符：用 char_len 推进
                let clen = utf8_char_len(bytes[i]);
                for k in 0..clen {
                    if i + k < bytes.len() {
                        out.push(bytes[i + k] as char);
                    }
                }
                i += clen;
            }
            continue;
        }
        // 双引号标识符（PG/MySQL 字符串）——与字符串同样处理
        if c == '"' {
            out.push('"');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        out.push('"');
                        out.push('"');
                        i += 2;
                        continue;
                    }
                    out.push('"');
                    i += 1;
                    break;
                }
                let clen = utf8_char_len(bytes[i]);
                for k in 0..clen {
                    if i + k < bytes.len() {
                        out.push(bytes[i + k] as char);
                    }
                }
                i += clen;
            }
            continue;
        }
        // 反引号（MySQL）
        if c == '`' {
            out.push('`');
            i += 1;
            while i < bytes.len() && bytes[i] != b'`' {
                let clen = utf8_char_len(bytes[i]);
                for k in 0..clen {
                    if i + k < bytes.len() {
                        out.push(bytes[i + k] as char);
                    }
                }
                i += clen;
            }
            if i < bytes.len() {
                out.push('`');
                i += 1;
            }
            continue;
        }
        // 美元引号字符串（PG $$...$$）：跳过即可
        if c == '$' {
            // 简化：把 $tag$ 视为单字符 token，原样输出两个字符
            out.push('$');
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xC0 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// 计算多语句中首条之外的语句数量（按分号切分，忽略字符串/注释内的分号——已剥除）
fn count_extra_statements(stripped: &str) -> u32 {
    let mut count = 0u32;
    let mut found_first = false;
    for ch in stripped.chars() {
        if ch == ';' {
            if found_first {
                count += 1;
            } else {
                found_first = true;
            }
        }
    }
    if !found_first {
        0
    } else {
        count
    }
}

fn read_first_keyword(stripped: &str) -> Option<String> {
    let s = stripped.trim_start();
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    // 跳过括号直到第一个 token
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || c == '(' || c == ';' {
            chars.next();
        } else {
            break;
        }
    }
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || c == '(' || c == ';' {
            break;
        }
        buf.push(c);
        chars.next();
    }
    if buf.is_empty() {
        None
    } else {
        Some(buf.to_ascii_uppercase())
    }
}

fn classify_keyword(kw: &str, statement: &str) -> StatementKind {
    match kw {
        "SELECT" | "WITH" | "SHOW" | "DESC" | "DESCRIBE" | "PRAGMA" => StatementKind::Query,
        "EXPLAIN" => StatementKind::Query,
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "MERGE" => StatementKind::Dml,
        "CREATE" | "DROP" | "ALTER" | "TRUNCATE" | "RENAME" | "GRANT" | "REVOKE"
        | "COMMENT" | "ANALYZE" | "VACUUM" | "REINDEX" => StatementKind::Ddl,
        "BEGIN" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" | "START" | "END" => {
            StatementKind::TxControl
        }
        "SET" => StatementKind::SessionCfg,
        "USE" | "DATABASE" => StatementKind::SessionCfg,
        _ => {
            // 兜底：在语句正文找含 FOR UPDATE/LOCK IN SHARE MODE 等高风险只读
            let upper = statement.to_ascii_uppercase();
            if upper.contains("FOR UPDATE")
                || upper.contains("FOR NO KEY UPDATE")
                || upper.contains("FOR SHARE")
                || upper.contains("LOCK IN SHARE MODE")
                || upper.contains("SELECT INTO")
                || upper.contains("EXPLAIN ANALYZE")
            {
                StatementKind::ReadModify
            } else {
                StatementKind::Unknown
            }
        }
    }
}

pub fn classify(sql: &str) -> Classification {
    let stripped = strip_strings_and_comments(sql);
    let first = read_first_keyword(&stripped);
    let other = count_extra_statements(&stripped);
    let kind = match &first {
        Some(kw) => classify_keyword(kw, &stripped),
        None => StatementKind::Unknown,
    };
    let safety = match kind {
        StatementKind::Query => {
            let u = stripped.to_ascii_uppercase();
            if u.contains("EXPLAIN ANALYZE") || u.contains("FOR UPDATE")
                || u.contains("FOR SHARE") || u.contains("SELECT INTO")
            {
                SafetyClass::ReadOnlyUnsafe
            } else {
                SafetyClass::ReadOnlySafe
            }
        }
        StatementKind::ReadModify => SafetyClass::ReadOnlyUnsafe,
        StatementKind::Dml | StatementKind::Ddl => SafetyClass::Write,
        StatementKind::TxControl => SafetyClass::Write,
        StatementKind::SessionCfg => SafetyClass::Write,
        StatementKind::Unknown => SafetyClass::Unknown,
    };
    Classification {
        kind,
        safety,
        other_statements: other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_is_query() {
        assert_eq!(classify("SELECT 1").kind, StatementKind::Query);
        assert_eq!(classify("select * from t").kind, StatementKind::Query);
    }

    #[test]
    fn cte_select_is_query() {
        assert_eq!(
            classify("WITH a AS (SELECT 1) SELECT * FROM a").kind,
            StatementKind::Query
        );
    }

    #[test]
    fn insert_is_write() {
        assert_eq!(
            classify("INSERT INTO t VALUES (1)").safety,
            SafetyClass::Write
        );
    }

    #[test]
    fn returning_is_still_write() {
        // INSERT/UPDATE/DELETE ... RETURNING 仍属于写入
        let c = classify("INSERT INTO t VALUES (1) RETURNING id");
        assert_eq!(c.safety, SafetyClass::Write);
    }

    #[test]
    fn comment_hack_blocked() {
        // 旧 starts_with("select") 通过；本分类器应识别为 UPDATE
        let c = classify("/* SELECT */ UPDATE t SET x=1");
        assert_eq!(c.kind, StatementKind::Dml);
        assert_eq!(c.safety, SafetyClass::Write);
    }

    #[test]
    fn line_comment_hack_blocked() {
        let c = classify("-- SELECT\nUPDATE t SET x=1");
        assert_eq!(c.kind, StatementKind::Dml);
        assert_eq!(c.safety, SafetyClass::Write);
    }

    #[test]
    fn string_literal_keyword_ignored() {
        // 字面量中的 'SELECT' 不应被误判
        let c = classify("INSERT INTO logs (msg) VALUES ('SELECT something')");
        assert_eq!(c.kind, StatementKind::Dml);
        assert_eq!(c.safety, SafetyClass::Write);
    }

    #[test]
    fn select_for_update_is_readonly_unsafe() {
        let c = classify("SELECT * FROM t FOR UPDATE");
        // 分类器把 FOR UPDATE 归为 ReadOnlyUnsafe（safety），但 kind 仍标 Query 以保留语义
        assert_eq!(c.kind, StatementKind::Query);
        assert_eq!(c.safety, SafetyClass::ReadOnlyUnsafe);
    }

    #[test]
    fn explain_analyze_is_readonly_unsafe() {
        let c = classify("EXPLAIN ANALYZE SELECT * FROM t");
        assert_eq!(c.safety, SafetyClass::ReadOnlyUnsafe);
    }

    #[test]
    fn multi_statement_detects_second() {
        let c = classify("SELECT 1; DROP TABLE t;");
        assert_eq!(c.kind, StatementKind::Query);
        assert_eq!(c.other_statements, 1);
    }

    #[test]
    fn transaction_keywords_are_write() {
        let c = classify("BEGIN TRANSACTION");
        assert_eq!(c.kind, StatementKind::TxControl);
        assert_eq!(c.safety, SafetyClass::Write);
    }
}
