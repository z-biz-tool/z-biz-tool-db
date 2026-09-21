// T-027：Rust 集成测试公共辅助模块
//
// 使用：
// - #[tokio::test] async fn t() { ... }
// - 临时数据目录：tests::fresh_data_dir() 注入 Z_BIZ_TOOL_DB_DATA_DIR
//
// MySQL/PG 集成测试放在 #[ignore] 标记下，CI 在 docker compose up 之后移除 ignore。
// SQLite 一律打开（不需标记），先在 SQLite 跑过的逻辑必先在 SQLite 内复现。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 每次测试返回独立临时目录；并自动注入 Z_BIZ_TOOL_DB_DATA_DIR
pub fn fresh_data_dir(label: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "zbiz-test-{}-{}-{}",
        label,
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&dir).expect("create tmp dir");
    let path_str = dir.to_string_lossy().to_string();
    if path_str.contains(' ') {
        panic!("临时目录含空格（SQLite/serde 等可能拒收）：{}", path_str);
    }
    std::env::set_var("Z_BIZ_TOOL_DB_DATA_DIR", &dir);
    dir
}

/// 检查 MySQL 是否可用于集成测试
pub fn mysql_available() -> bool {
    std::env::var("Z_BIZ_TOOL_DB_MYSQL_URL").is_ok()
}

/// 检查 PostgreSQL 是否可用于集成测试
pub fn pg_available() -> bool {
    std::env::var("Z_BIZ_TOOL_DB_PG_URL").is_ok()
}
