// T-027：SQLite 端集成测试入口
//
// 不依赖任何外部数据库，确保单元/集成基线在 PR 阶段可跑。
#[path = "mod.rs"]
mod common;

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
    // 默认无 Z_BIZ_TOOL_DB_MYSQL_URL 时应返回 false
    std::env::remove_var("Z_BIZ_TOOL_DB_MYSQL_URL");
    assert!(!common::mysql_available());
    std::env::set_var("Z_BIZ_TOOL_DB_MYSQL_URL", "mysql://root@127.0.0.1/test");
    assert!(common::mysql_available());
    std::env::remove_var("Z_BIZ_TOOL_DB_MYSQL_URL");
}
