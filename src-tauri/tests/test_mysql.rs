// T-027：MySQL 集成测试入口
//
// 需要外部 MySQL：
//   docker compose -f tests/docker-compose.test.yml up -d
//   export Z_BIZ_TOOL_DB_MYSQL_URL=mysql://root:test_password_do_not_use@127.0.0.1:3306/testdb
//   cargo test -- --ignored --test-threads=1
//
// 标 #[ignore] 的理由同 test_postgres.rs：常规跑批里没有 MySQL 服务。
#[path = "common/mod.rs"]
mod common;

use sqlx::mysql::MySqlPool;

/// MySQL 冒烟：建表 → 插入 → 读回 → 更新 → 删除
#[tokio::test]
#[ignore = "需要外部 MySQL：见文件头"]
async fn mysql_smoke_crud() {
    let url = std::env::var("Z_BIZ_TOOL_DB_MYSQL_URL")
        .expect("Z_BIZ_TOOL_DB_MYSQL_URL 未设置；按文件头起 compose 再跑");
    let pool = MySqlPool::connect(&url).await.expect("mysql connect");

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS smoke_mysql (
            id INT PRIMARY KEY,
            int_val INT,
            real_val DOUBLE,
            text_val VARCHAR(64),
            null_val VARCHAR(64)
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");
    sqlx::query("DELETE FROM smoke_mysql")
        .execute(&pool)
        .await
        .expect("clean");

    sqlx::query(
        "INSERT INTO smoke_mysql (id, int_val, real_val, text_val, null_val)
         VALUES (1, 42, 12.34, 'hello', NULL)",
    )
    .execute(&pool)
    .await
    .expect("insert");

    let row: (
        i32,
        Option<i32>,
        Option<f64>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT id, int_val, real_val, text_val, null_val FROM smoke_mysql WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("select");
    assert_eq!(row.0, 1);
    assert_eq!(row.1, Some(42));
    assert!((row.2.unwrap() - 12.34).abs() < 0.001);
    assert_eq!(row.3.as_deref(), Some("hello"));
    assert!(row.4.is_none());

    sqlx::query("UPDATE smoke_mysql SET int_val = -1000 WHERE id = 1")
        .execute(&pool)
        .await
        .expect("update");
    let updated: i32 = sqlx::query_scalar("SELECT int_val FROM smoke_mysql WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("select updated");
    assert_eq!(updated, -1000);

    sqlx::query("DELETE FROM smoke_mysql WHERE id = 1")
        .execute(&pool)
        .await
        .expect("delete");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM smoke_mysql")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 0);
}
