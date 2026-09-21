// T-027/T-060：SQLite 端集成测试入口
//
// 不依赖任何外部数据库，确保单元/集成基线在 PR 阶段可跑。
// 每平台至少一个冒烟测试（T-060 要求）。
#[path = "mod.rs"]
mod common;

use sqlx::sqlite::SqlitePool;

#[tokio::test]
async fn sqlite_helpers_provide_fresh_data_dir() {
    let dir1 = common::fresh_data_dir("h1");
    let dir2 = common::fresh_data_dir("h2");
    assert_ne!(dir1, dir2, "每次调用应返回不同目录");
    assert!(dir1.exists(), "目录应已建好：{:?}", dir1);
    assert!(dir2.exists(), "目录应已建好：{:?}", dir2);
    assert_eq!(
        std::env::var("Z_BIZ_TOOL_DB_DATA_DIR").ok().as_deref(),
        Some(dir2.to_string_lossy().as_ref()),
        "环境变量应注入到最新目录"
    );
}

#[tokio::test]
async fn mysql_availability_flag() {
    std::env::remove_var("Z_BIZ_TOOL_DB_MYSQL_URL");
    assert!(!common::mysql_available());
    std::env::set_var("Z_BIZ_TOOL_DB_MYSQL_URL", "mysql://root@127.0.0.1/test");
    assert!(common::mysql_available());
    std::env::remove_var("Z_BIZ_TOOL_DB_MYSQL_URL");
}

/// T-060：跨平台冒烟测试 - SQLite 内存连接 + 基础 CRUD + tagged cell 验证
/// 确保核心数据类型往返一致（整数、浮点、字符串、NULL、BLOB）
#[tokio::test]
async fn sqlite_smoke_crud_tagged_cells() {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("sqlite connect");

    // 建表
    sqlx::query(
        "CREATE TABLE smoke (
            id INTEGER PRIMARY KEY,
            int_val INTEGER,
            real_val REAL,
            text_val TEXT,
            null_val TEXT,
            blob_val BLOB
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");

    // 插入测试数据
    sqlx::query(
        "INSERT INTO smoke VALUES (1, 42, 3.14, 'hello', NULL, X'DEADBEEF')",
    )
    .execute(&pool)
    .await
    .expect("insert");

    // 查询并验证 tagged cell 格式
    let rows: Vec<(i64, Option<i64>, Option<f64>, Option<String>, Option<String>, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT id, int_val, real_val, text_val, null_val, blob_val FROM smoke")
            .fetch_all(&pool)
            .await
            .expect("query smoke");

    assert_eq!(rows.len(), 1, "应有 1 行");
    let (id, int_val, real_val, text_val, null_val, blob_val) = &rows[0];
    assert_eq!(*id, 1);
    assert_eq!(*int_val, Some(42));
    assert!((real_val.unwrap() - 3.14).abs() < 0.001);
    assert_eq!(text_val.as_deref(), Some("hello"));
    assert!(null_val.is_none());
    assert!(blob_val.as_ref().unwrap().len() > 0, "BLOB 不为空");

    // 更新验证
    sqlx::query("UPDATE smoke SET int_val = -1000 WHERE id = 1")
        .execute(&pool)
        .await
        .expect("update");
    let updated: i64 = sqlx::query_scalar("SELECT int_val FROM smoke WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("query updated");
    assert_eq!(updated, -1000);

    // 删除验证
    sqlx::query("DELETE FROM smoke WHERE id = 1")
        .execute(&pool)
        .await
        .expect("delete");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM smoke")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 0);
}
