// T-042: 性能基准建立
//
// 使用 SQLite 内存库建立基准指标：
// - 100000 行 × 20 列合成数据
// - 记录查询耗时
// - 基线参考：首批 p95 ≤ 1s，RSS ≤ 200 MiB

use std::time::Instant;
use sqlx::sqlite::SqlitePool;

/// 生成合成数据并执行基准测试
async fn run_benchmark() {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("sqlite connect");

    // 创建表
    sqlx::query(
        "CREATE TABLE benchmark (
            id INTEGER PRIMARY KEY,
            col1 TEXT, col2 INTEGER, col3 REAL, col4 TEXT,
            col5 INTEGER, col6 REAL, col7 TEXT, col8 INTEGER,
            col9 REAL, col10 TEXT, col11 INTEGER, col12 REAL,
            col13 TEXT, col14 INTEGER, col15 REAL, col16 TEXT,
            col17 INTEGER, col18 REAL, col19 TEXT, col20 INTEGER
        )"
    )
    .execute(&pool)
    .await
    .expect("create table");

    // 插入 100000 行
    let start = Instant::now();
    for batch in 0..100 {
        let mut values = Vec::new();
        for i in 0..1000 {
            let row = batch * 1000 + i;
            let a = row % 1000;
            let b = row * 2;
            let c = row % 500;
            let d = row * 3;
            let e = row % 250;
            let f = row * 4;
            let g = row % 100;
            let h = row * 5;
            let j = row % 50;
            let k = row * 6;
            let m = row % 25;
            let n = row * 7;
            let p = row % 15;
            let q = row * 8;
            let s = row % 12;
            let t = row * 9;
            let u = row % 8;
            let v = row * 10;
            values.push(format!("({},'v{}',{},1.0,'t{}',{},1.0,'d{}',{},1.0,'i{}',{},1.0,'s{}',{},1.0,'r{}',{},1.0,'f{}',{},1.0,'g{}',{},1.0,'h{}',{})", row, a, b, c, d, e, f, g, h, j, k, m, n, p, q, s, t, u, v));
        }
        let sql = format!("INSERT INTO benchmark VALUES {}", values.join(","));
        sqlx::query(&sql).execute(&pool).await.expect("insert batch");
    }
    let insert_time = start.elapsed().as_millis();
    println!("[T-042] Insert 100000 rows: {}ms", insert_time);

    // 基准测试：全表扫描
    let start = Instant::now();
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM benchmark")
        .fetch_all(&pool)
        .await
        .expect("count");
    let count = rows[0].0;
    println!("[T-042] SELECT COUNT: {}ms, rows={}", start.elapsed().as_millis(), count);
    assert_eq!(count, 100000);

    // 基准测试：索引查询
    let start = Instant::now();
    for i in 0..1000 {
        let _: Vec<(i64, String)> = sqlx::query_as("SELECT id, col1 FROM benchmark WHERE id = ?")
            .bind(i)
            .fetch_all(&pool)
            .await
            .expect("index query");
    }
    println!("[T-042] 1000 indexed queries: {}ms", start.elapsed().as_millis());

    // 基准测试：范围查询
    let start = Instant::now();
    let _: Vec<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM benchmark WHERE col2 BETWEEN 1000 AND 2000")
        .fetch_all(&pool)
        .await
        .expect("range query");
    println!("[T-042] Range query: {}ms", start.elapsed().as_millis());

    println!("[T-042] Benchmark complete");
}

#[tokio::main]
async fn main() {
    run_benchmark().await;
}
