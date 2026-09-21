// T-011：ConnectionManager
//
// 以 (connection_id, revision, access_mode) 为键缓存驱动连接池：
// - 同一连接多次查询复用池，符合 [03 架构] A04 验收
// - 池上限 16 条；超出时按最近空闲优先回收
// - 写操作走 "writable" 池，只读走 "readOnly" 池；两者身份不共享
//
// S0 范围：仅作缓存层封装，不替代 sqlx 连接池自己的安全语义；
// revision 现有 DBConfig.id 即可近似——S1 完整实现 revision CAS 时再扩展。

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub connection_id: String,
    pub revision: u32,
    pub access_mode: String, // "readOnly" | "writable"
}

pub trait Pool: Send + Sync {
    fn kind(&self) -> &'static str;
}

impl Pool for sqlx::MySqlPool {
    fn kind(&self) -> &'static str {
        "mysql"
    }
}
impl Pool for sqlx::PgPool {
    fn kind(&self) -> &'static str {
        "postgresql"
    }
}
impl Pool for sqlx::SqlitePool {
    fn kind(&self) -> &'static str {
        "sqlite"
    }
}

#[derive(Clone, Debug)]
pub enum AnyPool {
    MySql(sqlx::MySqlPool),
    Pg(sqlx::PgPool),
    Sqlite(sqlx::SqlitePool),
}

impl AnyPool {
    pub fn kind(&self) -> &'static str {
        match self {
            AnyPool::MySql(_) => "mysql",
            AnyPool::Pg(_) => "postgresql",
            AnyPool::Sqlite(_) => "sqlite",
        }
    }
}

struct Entry {
    pool: AnyPool,
    last_used: Instant,
}

#[derive(Default)]
pub struct ConnectionManager {
    inner: Mutex<HashMap<PoolKey, Entry>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 上限缓存条数；超出时按最久未用淘汰。
    const MAX_ENTRIES: usize = 16;

    pub async fn get_or_insert<F, Fut>(
        &self,
        key: PoolKey,
        ctor: F,
    ) -> Result<AnyPool, String>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<AnyPool, String>> + Send,
    {
        let mut guard = self.inner.lock().await;
        if let Some(entry) = guard.get_mut(&key) {
            entry.last_used = Instant::now();
            return Ok(entry.pool.clone());
        }
        drop(guard);
        let pool = ctor().await?;
        let mut guard = self.inner.lock().await;
        // 二次检查防止并发插入
        if let Some(entry) = guard.get_mut(&key) {
            entry.last_used = Instant::now();
            return Ok(entry.pool.clone());
        }
        if guard.len() >= Self::MAX_ENTRIES {
            // 找到最久未用的 entry 淘汰
            if let Some(oldest_key) = guard
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest_key);
            }
        }
        guard.insert(
            key.clone(),
            Entry {
                pool,
                last_used: Instant::now(),
            },
        );
        Ok(guard.get(&key).expect("刚插入").pool.clone())
    }

    pub async fn idle_sweep(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let mut guard = self.inner.lock().await;
        let before = guard.len();
        guard.retain(|_, e| now.duration_since(e.last_used) < ttl);
        before - guard.len()
    }

    pub async fn clear(&self) {
        self.inner.lock().await.clear();
    }

    pub async fn stats(&self) -> (usize, usize) {
        let guard = self.inner.lock().await;
        let mut by_kind: HashMap<&'static str, usize> = HashMap::new();
        for entry in guard.values() {
            *by_kind.entry(entry.pool.kind()).or_insert(0) += 1;
        }
        (guard.len(), by_kind.values().sum())
    }
}

// 全局单例；Tauri 进程一个。用 std::sync::OnceLock 避免引入新依赖。
use std::sync::OnceLock;

static POOL_REGISTRY_CELL: OnceLock<std::sync::Arc<ConnectionManager>> = OnceLock::new();

pub fn pool_registry() -> std::sync::Arc<ConnectionManager> {
    POOL_REGISTRY_CELL
        .get_or_init(|| std::sync::Arc::new(ConnectionManager::new()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn reuse_pool_caches_call() {
        let mgr = ConnectionManager::new();
        let key = PoolKey {
            connection_id: "c1".into(),
            revision: 0,
            access_mode: "readOnly".into(),
        };
        let counter = StdArc::new(AtomicUsize::new(0));
        let c1 = counter.clone();
        let p1 = mgr
            .get_or_insert(key.clone(), || async move {
                c1.fetch_add(1, Ordering::SeqCst);
                Ok(AnyPool::Sqlite(
                    sqlx::SqlitePool::connect("sqlite::memory:")
                        .await
                        .map_err(|e| e.to_string())?,
                ))
            })
            .await
            .unwrap();

        let c2 = counter.clone();
        let p2 = mgr
            .get_or_insert(key, || async move {
                c2.fetch_add(1, Ordering::SeqCst);
                Ok(AnyPool::Sqlite(
                    sqlx::SqlitePool::connect("sqlite::memory:")
                        .await
                        .map_err(|e| e.to_string())?,
                ))
            })
            .await
            .unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 1, "第二次应命中缓存而非重建");
        assert_eq!(p1.kind(), p2.kind(), "两次缓存应返回相同 kind");
    }

    #[tokio::test]
    async fn different_keys_get_different_pools() {
        let mgr = ConnectionManager::new();
        let mk = |id: &str, mode: &str| PoolKey {
            connection_id: id.into(),
            revision: 0,
            access_mode: mode.into(),
        };
        let p1 = mgr
            .get_or_insert(mk("a", "readOnly"), || async {
                Ok(AnyPool::Sqlite(
                    sqlx::SqlitePool::connect("sqlite::memory:")
                        .await
                        .map_err(|e| e.to_string())?,
                ))
            })
            .await
            .unwrap();
        let p2 = mgr
            .get_or_insert(mk("a", "writable"), || async {
                Ok(AnyPool::Sqlite(
                    sqlx::SqlitePool::connect("sqlite::memory:")
                        .await
                        .map_err(|e| e.to_string())?,
                ))
            })
            .await
            .unwrap();
        // 缓存计数至少 +2，但 SQLite 内存池无法直接严格比对 ID；
        // 验证 stats 至少 2 条
        assert!(mgr.stats().await.0 >= 2);
        assert_eq!(p1.kind(), p2.kind());
    }

    #[tokio::test]
    async fn idle_sweep_drops_unused() {
        let mgr = ConnectionManager::new();
        let key = PoolKey {
            connection_id: "sweep".into(),
            revision: 0,
            access_mode: "readOnly".into(),
        };
        mgr.get_or_insert(key, || async {
            Ok(AnyPool::Sqlite(
                sqlx::SqlitePool::connect("sqlite::memory:")
                    .await
                    .map_err(|e| e.to_string())?,
            ))
        })
        .await
        .unwrap();
        // 用极短 TTL 模拟久未使用
        std::thread::sleep(Duration::from_millis(60));
        let dropped = mgr.idle_sweep(Duration::from_millis(20)).await;
        assert!(dropped >= 1, "应回收至少 1 条：{}", dropped);
    }
}
