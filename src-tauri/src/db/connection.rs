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
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub connection_id: String,
    /// 连接**参数指纹**：host/port/username/password/database/TLS 任一变化都会变。
    ///
    /// 为什么不是"连接 id"：用户改了 host 之后，缓存里那个旧池还指着旧库，
    /// 照旧 id 取池 ⇒ **静默连到错误的库**。指纹变了就等于换了一条池。
    ///
    /// 密码也参与指纹（否则改密码后旧池仍带旧凭据 ⇒ 认证失败），
    /// 但**只留摘要不留明文**，且摘要带进程内随机盐 ⇒ 不是可离线爆破的凭据材料。
    pub fingerprint: u32,
    pub access_mode: String, // "readOnly" | "writable"
}

/// 进程内随机盐：指纹的输入之一。进程重启即换 ⇒ 内存里的摘要不可跨进程复用。
fn fingerprint_salt() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SALT: AtomicU32 = AtomicU32::new(0);
    let mut s = SALT.load(Ordering::Relaxed);
    if s == 0 {
        s = u32::from_le_bytes(
            uuid::Uuid::new_v4().into_bytes()[0..4]
                .try_into()
                .expect("uuid 4 字节"),
        ) | 1;
        SALT.store(s, Ordering::Relaxed);
    }
    s
}

/// FNV-1a 32 位。选它是因为够快且**不引入新依赖**（本仓不为了一个指纹拉一个哈希库）。
fn fnv1a(bytes: &[u8], seed: u32) -> u32 {
    let mut h = seed;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// 连接参数指纹：参与建池的每一个参数都进去。
///
/// 刻意**不**包含 `name`（显示名与连哪个库无关）与 `id`（id 在 PoolKey 里单列）。
pub fn fingerprint(config: &crate::DBConfig) -> u32 {
    let mut h = fnv1a(&fingerprint_salt().to_le_bytes(), 0x811c_9dc5);
    let mut feed = |s: &str| {
        h = fnv1a(s.as_bytes(), h);
        h = fnv1a(&[0u8], h); // 分隔符，避免 "ab"+"c" 与 "a"+"bc" 撞
    };
    feed(&config.host);
    feed(&config.port.to_string());
    feed(&config.username);
    feed(&config.password);
    feed(&config.database);
    feed(&config.db_type);
    match &config.tls_mode {
        Some(m) => feed(m.as_str()),
        None => feed(""),
    }
    match &config.tls_ca_path {
        Some(p) => feed(p),
        None => feed(""),
    }
    h
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
    /// 池的种类。走 `Pool` trait（三个 sqlx 池各自实现了它），
    /// 而不是在这里另写一份 match —— 两份会漂。
    pub fn kind(&self) -> &'static str {
        match self {
            AnyPool::MySql(p) => Pool::kind(p),
            AnyPool::Pg(p) => Pool::kind(p),
            AnyPool::Sqlite(p) => Pool::kind(p),
        }
    }
}

struct Entry {
    pool: AnyPool,
    last_used: Instant,
}

#[derive(Default)]
pub struct ConnectionManager {
    /// 事务专属池：一个进行中的数据库事务必须跑在**同一条连接**上。
    ///
    /// 普通池有多条连接 ⇒ `BEGIN` 与 `COMMIT` 会落到不同连接 ⇒
    /// 数据库端根本没有这个事务（COMMIT 报 "no transaction is active"，
    /// 或在 MySQL 上悄悄提交了一个空事务）。所以事务期间改用这里的
    /// **单连接池**，事务结束（COMMIT/ROLLBACK/失败）即丢弃。
    tx_pools: Mutex<HashMap<String, AnyPool>>,
    /// 用 `std::sync::Mutex` 而不是 `tokio::sync::Mutex`：
    /// 临界区只有 HashMap 增删查（微秒级），**任何时候都不跨 await**
    /// （`get_or_insert` 在 await 构造池之前显式 `drop(guard)`）。
    /// 换成同步锁的好处是 `clear()` 可以在应用退出钩子里**同步**调用 ——
    /// tokio 的锁在 runtime 关闭阶段 await 是不安全的。
    inner: Mutex<HashMap<PoolKey, Entry>>,
}

/// 取锁；毒了就当没有毒（池缓存丢了重建即可，不值得因此崩掉整个应用）。
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 上限缓存条数；超出时按最久未用淘汰。
    const MAX_ENTRIES: usize = 16;

    pub async fn get_or_insert<F, Fut>(&self, key: PoolKey, ctor: F) -> Result<AnyPool, String>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<AnyPool, String>> + Send,
    {
        {
            let mut guard = lock(&self.inner);
            if let Some(entry) = guard.get_mut(&key) {
                entry.last_used = Instant::now();
                return Ok(entry.pool.clone());
            }
        }
        // 关键：构造池期间**不持锁**（否则所有查询都串行化在建连接上）。
        let pool = ctor().await?;
        let mut guard = lock(&self.inner);
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

    pub fn idle_sweep(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let mut guard = lock(&self.inner);
        let before = guard.len();
        guard.retain(|_, e| now.duration_since(e.last_used) < ttl);
        before - guard.len()
    }

    /// 关掉**全部**池。
    ///
    /// 生产落点：应用退出时（`RunEvent::Exit`）主动释放，让数据库端看到干净的断开，
    /// 而不是等 TCP 超时。之所以能同步调用，是因为内部用的是 `std::sync::Mutex`
    /// 且临界区里没有 await。
    pub fn clear(&self) {
        lock(&self.inner).clear();
    }

    /// 精确关掉**某条连接**的池；返回关掉了几条。
    ///
    /// 为什么必须有这个：原来的 `close_pool` 命令直接 `clear()` ——
    /// 关一条连接会把**所有**连接（含别人的）的池一起清空，
    /// 正在跑的查询随即报"连接已关闭"。`close_pool` 的两个入参
    /// （connection_id / revision）当时是被 `let _ =` 丢掉的。
    pub fn remove_connection(&self, connection_id: &str) -> usize {
        let mut guard = lock(&self.inner);
        let before = guard.len();
        guard.retain(|k, _| k.connection_id != connection_id);
        before - guard.len()
    }

    /// 当前缓存的池数与按种类分布。前端没调用这个命令（0 处），
    /// 但它是「池复用了没有」唯一能直接回答的口子。
    pub fn stats(&self) -> (usize, usize) {
        let guard = lock(&self.inner);
        let mut by_kind: HashMap<&'static str, usize> = HashMap::new();
        for entry in guard.values() {
            *by_kind.entry(entry.pool.kind()).or_insert(0) += 1;
        }
        (guard.len(), by_kind.values().sum())
    }

    // ---------- 事务专属池 ----------

    /// 该连接当前是否有一个进行中的事务（⇒ 语句要走单连接池）
    pub fn has_tx_pool(&self, connection_id: &str) -> bool {
        lock(&self.tx_pools).contains_key(connection_id)
    }

    /// 取出该连接的事务池（没有则 None）
    pub fn tx_pool(&self, connection_id: &str) -> Option<AnyPool> {
        lock(&self.tx_pools).get(connection_id).cloned()
    }

    /// 登记该连接的事务池
    pub fn insert_tx_pool(&self, connection_id: &str, pool: AnyPool) {
        lock(&self.tx_pools).insert(connection_id.to_string(), pool);
    }

    /// 丢弃该连接的事务池（事务结束）⇒ 后续语句回到普通池
    pub fn remove_tx_pool(&self, connection_id: &str) -> Option<AnyPool> {
        lock(&self.tx_pools).remove(connection_id)
    }

    /// 按种类给出缓存分布（诊断用）。
    pub fn stats_by_kind(&self) -> Vec<(String, usize)> {
        let guard = lock(&self.inner);
        let mut by_kind: HashMap<&'static str, usize> = HashMap::new();
        for entry in guard.values() {
            *by_kind.entry(entry.pool.kind()).or_insert(0) += 1;
        }
        let mut v: Vec<(String, usize)> = by_kind
            .into_iter()
            .map(|(k, n)| (k.to_string(), n))
            .collect();
        v.sort();
        v
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
            fingerprint: 0,
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

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "第二次应命中缓存而非重建"
        );
        assert_eq!(p1.kind(), p2.kind(), "两次缓存应返回相同 kind");
    }

    #[tokio::test]
    async fn different_keys_get_different_pools() {
        let mgr = ConnectionManager::new();
        let mk = |id: &str, mode: &str| PoolKey {
            connection_id: id.into(),
            fingerprint: 0,
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
        assert!(mgr.stats().0 >= 2);
        assert_eq!(p1.kind(), p2.kind());
    }

    #[tokio::test]
    async fn idle_sweep_drops_unused() {
        let mgr = ConnectionManager::new();
        let key = PoolKey {
            connection_id: "sweep".into(),
            fingerprint: 0,
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
        let dropped = mgr.idle_sweep(Duration::from_millis(20));
        assert!(dropped >= 1, "应回收至少 1 条：{}", dropped);
    }

    /// 参数指纹：host/username/password/database/TLS 任一变化，指纹必须变。
    ///
    /// 这条是"改了 host 不会静默连到旧库"的根：指纹变了 ⇒ 池的键变了 ⇒ 拿到新池。
    #[test]
    fn fingerprint_changes_with_every_pool_affecting_parameter() {
        let base = crate::DBConfig {
            id: "c1".into(),
            name: "n".into(),
            db_type: "postgresql".into(),
            host: "db.internal".into(),
            port: 5432,
            username: "u".into(),
            password: "p".into(),
            database: "d".into(),
            tls_mode: None,
            tls_ca_path: None,
        };
        let fp = fingerprint(&base);
        // 同样的参数 ⇒ 同样的指纹（否则每次查询都开新池，复用就没了）
        assert_eq!(fp, fingerprint(&base));

        let mut c = base.clone();
        c.host = "other.internal".into();
        assert_ne!(fp, fingerprint(&c), "改 host 必须换池");

        let mut c = base.clone();
        c.port = 5433;
        assert_ne!(fp, fingerprint(&c), "改 port 必须换池");

        let mut c = base.clone();
        c.username = "other".into();
        assert_ne!(fp, fingerprint(&c), "改 username 必须换池");

        let mut c = base.clone();
        c.password = "other".into();
        assert_ne!(fp, fingerprint(&c), "改 password 必须换池（旧池带旧凭据）");

        let mut c = base.clone();
        c.database = "other".into();
        assert_ne!(fp, fingerprint(&c), "改 database 必须换池");

        let mut c = base.clone();
        c.tls_mode = Some(crate::TlsMode::Disable);
        assert_ne!(fp, fingerprint(&c), "改 TLS 档位必须换池");

        // 只改显示名不该换池（名字与连哪个库无关）
        let mut c = base.clone();
        c.name = "改个名".into();
        assert_eq!(fp, fingerprint(&c), "显示名不影响池身份");
    }

    /// close_pool 的新语义：只关这一条连接的池，别人的池必须留着。
    #[tokio::test]
    async fn remove_connection_only_drops_that_connection() {
        let mgr = ConnectionManager::new();
        let mk = |id: &str, mode: &str| PoolKey {
            connection_id: id.into(),
            fingerprint: 0,
            access_mode: mode.into(),
        };
        for (id, mode) in [("a", "readOnly"), ("a", "writable"), ("b", "readOnly")] {
            mgr.get_or_insert(mk(id, mode), || async {
                Ok(AnyPool::Sqlite(
                    sqlx::SqlitePool::connect("sqlite::memory:")
                        .await
                        .map_err(|e| e.to_string())?,
                ))
            })
            .await
            .unwrap();
        }
        assert_eq!(mgr.stats().0, 3);
        let dropped = mgr.remove_connection("a");
        assert_eq!(dropped, 2, "a 的两条（只读+可写）都该被关掉");
        assert_eq!(mgr.stats().0, 1, "b 的池必须留着");
    }
}
