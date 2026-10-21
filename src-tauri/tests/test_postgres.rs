// T-027：PostgreSQL 集成测试入口
//
// 需要外部 PG：
//   docker compose -f tests/docker-compose.test.yml up -d
//   export Z_BIZ_TOOL_DB_PG_URL=postgres://postgres:test_password_do_not_use@127.0.0.1:5432/testdb
//   cargo test -- --ignored --test-threads=1
//
// 为什么标 #[ignore]：CI 的常规跑批里没有 PG 服务（起 docker 太慢且不稳）。
// 这些用例**必须**标 ignore —— 标了但从没跑过，等于没写；
// 所以它们的存在由本地 compose + 手动跑批保证，跑法写在本文件头与 compose 注释里。
#[path = "common/mod.rs"]
mod common;

use sqlx::postgres::PgPool;

/// PG 可用性开关（与 MySQL 同款契约：环境变量在 ⇒ 可用）
#[tokio::test]
async fn pg_availability_flag() {
    std::env::remove_var("Z_BIZ_TOOL_DB_PG_URL");
    assert!(!common::pg_available());
    std::env::set_var(
        "Z_BIZ_TOOL_DB_PG_URL",
        "postgres://postgres:pw@127.0.0.1:5432/testdb",
    );
    assert!(common::pg_available());
    std::env::remove_var("Z_BIZ_TOOL_DB_PG_URL");
}

/// PG 冒烟：建表 → 插入 → 读回 → 更新 → 删除
#[tokio::test]
#[ignore = "需要外部 PG：见文件头"]
async fn postgres_smoke_crud() {
    let url = std::env::var("Z_BIZ_TOOL_DB_PG_URL")
        .expect("Z_BIZ_TOOL_DB_PG_URL 未设置；按文件头起 compose 再跑");
    let pool = PgPool::connect(&url).await.expect("pg connect");

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS smoke_pg (
            id INTEGER PRIMARY KEY,
            int_val INTEGER,
            real_val DOUBLE PRECISION,
            text_val TEXT,
            null_val TEXT
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");
    sqlx::query("DELETE FROM smoke_pg")
        .execute(&pool)
        .await
        .expect("clean");

    sqlx::query(
        "INSERT INTO smoke_pg (id, int_val, real_val, text_val, null_val)
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
        "SELECT id, int_val, real_val, text_val, null_val FROM smoke_pg WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("select");
    assert_eq!(row.0, 1);
    assert_eq!(row.1, Some(42));
    assert!((row.2.unwrap() - 12.34).abs() < 0.001);
    assert_eq!(row.3.as_deref(), Some("hello"));
    assert!(row.4.is_none());

    sqlx::query("UPDATE smoke_pg SET int_val = -1000 WHERE id = 1")
        .execute(&pool)
        .await
        .expect("update");
    let updated: i32 = sqlx::query_scalar("SELECT int_val FROM smoke_pg WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("select updated");
    assert_eq!(updated, -1000);

    sqlx::query("DELETE FROM smoke_pg WHERE id = 1")
        .execute(&pool)
        .await
        .expect("delete");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM smoke_pg")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 0);
}
