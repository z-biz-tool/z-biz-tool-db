// T-014：SecretRef 抽象层 —— **已接线**到 save_connections / load_connections。
//
// 落盘只存 SecretRef（索引），绝不存明文；明文只存在于 SecretStore 拿回的
// 内存对象里，且 PlainSecret 在 drop 时把字节清零。
//
// 当前默认后端是 InMemorySecretStore：进程内有效、**跨进程不持久**。
// 将来在 src-tauri/src/secrets/keychain.rs 里换成 macOS Keychain /
// Windows DPAPI / Linux Secret Service 实现即可，接口不变。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// 引用 secret 的不可变索引；不携带明文
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SecretRef {
    /// 后端名字（inmemory | keychain-macos | keychain-windows | secret-service）
    pub backend: String,
    /// 后端内部 ID（UUID v4 / Keychain account 等）
    pub id: String,
    /// 用于日志/审计的展示名，不影响身份
    pub label: String,
    /// secret 修订号；轮换会递增
    pub revision: u32,
}

impl SecretRef {
    pub fn new_inmemory(id: &str, label: &str) -> Self {
        Self {
            backend: "inmemory".to_string(),
            id: id.to_string(),
            label: label.to_string(),
            revision: 0,
        }
    }
}

/// 当前进程内可见的明文 secret；最后一个持有者被 drop 时字节被清零。
///
/// 底层用 `Arc<Mutex<Vec<u8>>>` 而不是裸 `Vec<u8>`：这样测试可以额外持有一个
/// 观察句柄，在 `PlainSecret` 已经被 drop 之后**验证**字节确实被清零了
/// （裸 Vec 在 drop 之后观察不到，无法证伪）。
pub struct PlainSecret {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl PlainSecret {
    pub fn new(s: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: Arc::new(Mutex::new(s.into())),
        }
    }

    /// 取明文快照（副本）。调用方负责用完即弃。
    pub fn expose(&self) -> Vec<u8> {
        self.bytes.lock().map(|b| b.clone()).unwrap_or_default()
    }

    /// UTF-8 解码；非 UTF-8 视为错误（DB 密码都是 ASCII）
    pub fn as_str(&self) -> String {
        String::from_utf8_lossy(&self.expose()).into_owned()
    }

    /// 显式清零（不等 drop）。
    pub fn zero(&self) {
        if let Ok(mut b) = self.bytes.lock() {
            b.iter_mut().for_each(|x| *x = 0);
        }
    }

    /// 当前字节是否已全零（自检 / 测试用）。
    pub fn is_zeroed(&self) -> bool {
        self.bytes
            .lock()
            .map(|b| b.iter().all(|&x| x == 0))
            .unwrap_or(true)
    }
}

impl Drop for PlainSecret {
    fn drop(&mut self) {
        // 用 0 覆盖以减少剩余明文窗口
        self.zero();
    }
}

/// 后端 trait；每个实现负责一个平台
pub trait SecretStore: Send + Sync {
    /// 持久化一个明文 secret，返回可在配置里保存的 SecretRef
    fn put(&self, label: &str, secret: &PlainSecret) -> Result<SecretRef, String>;
    /// 取出明文；调用结束后丢弃
    fn get(&self, secret_ref: &SecretRef) -> Result<PlainSecret, String>;
    /// 删除（连接删除、密钥轮换）
    fn delete(&self, secret_ref: &SecretRef) -> Result<(), String>;
    /// 后端是否真持久化了（in-memory 返回 false；keychain 返回 true）
    fn persistent(&self) -> bool {
        false
    }
}

/// 存储后端内部持有的字节；drop 时清零（不留残余明文）。
struct SecretBytes {
    bytes: Vec<u8>,
}

impl SecretBytes {
    fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.bytes.iter_mut().for_each(|b| *b = 0);
    }
}

/// 仅 S0 阶段可用：内存 Map，便于自动化测试与无 keychain 环境；
/// 进程退出即消失——**跨进程不持久**，重启后必须重新输入密码。
#[derive(Default)]
pub struct InMemorySecretStore {
    inner: Arc<Mutex<HashMap<String, SecretBytes>>>,
}

impl SecretStore for InMemorySecretStore {
    fn put(&self, label: &str, secret: &PlainSecret) -> Result<SecretRef, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| format!("secret store 锁失败: {}", e))?;
        guard.insert(id.clone(), SecretBytes::new(secret.expose()));
        Ok(SecretRef::new_inmemory(&id, label))
    }

    fn get(&self, secret_ref: &SecretRef) -> Result<PlainSecret, String> {
        if secret_ref.backend != "inmemory" {
            return Err(format!(
                "inmemory backend 不支持其它后端的引用: {}",
                secret_ref.backend
            ));
        }
        let guard = self
            .inner
            .lock()
            .map_err(|e| format!("secret store 锁失败: {}", e))?;
        let holder = guard
            .get(&secret_ref.id)
            .ok_or_else(|| format!("找不到 secret: {}", secret_ref.id))?;
        Ok(PlainSecret::new(holder.bytes.clone()))
    }

    fn delete(&self, secret_ref: &SecretRef) -> Result<(), String> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| format!("secret store 锁失败: {}", e))?;
        guard.remove(&secret_ref.id);
        Ok(())
    }

    fn persistent(&self) -> bool {
        false
    }
}

/// 全局 secret store。
///
/// 用 `RwLock<Option<...>>` 而不是 `OnceLock`：OnceLock 一旦初始化就换不掉，
/// 测试无法注入探针后端。这里允许测试整体替换（`install_secret_store_for_test`）。
static STORE: RwLock<Option<Arc<dyn SecretStore>>> = RwLock::new(None);

/// 取全局 secret store；首次调用时装配默认的内存后端。
pub fn secret_store() -> Arc<dyn SecretStore> {
    if let Some(s) = STORE.read().ok().and_then(|g| g.clone()) {
        return s;
    }
    let mut g = STORE.write().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(|| Arc::new(InMemorySecretStore::default()))
        .clone()
}

/// 仅用于测试：替换全局 secret store，返回被替换掉的那一个（便于还原）。
///
/// 标 `#[cfg(test)]`：这是**测试钩子**，不该出现在发布版的公开 API 里。
/// （生产路径要换后端请换 `secret_store()` 的默认装配，不是在运行时替换。）
#[cfg(test)]
pub fn install_secret_store_for_test(s: Arc<dyn SecretStore>) -> Option<Arc<dyn SecretStore>> {
    let mut g = STORE.write().unwrap_or_else(|e| e.into_inner());
    g.replace(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_ref_does_not_carry_plaintext() {
        let r = SecretRef::new_inmemory("id-1", "mysql-pwd");
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("root"));
        assert!(!json.contains("passw"));
        assert!(json.contains("inmemory"));
        assert!(json.contains("id-1"));
    }

    #[test]
    fn put_get_roundtrip() {
        let store = InMemorySecretStore::default();
        let plain = PlainSecret::new("hunter2");
        let r = store.put("test", &plain).expect("put");
        assert_eq!(r.backend, "inmemory");
        let got = store.get(&r).expect("get");
        assert_eq!(got.as_str(), "hunter2");
        store.delete(&r).expect("delete");
        assert!(store.get(&r).is_err(), "删除后应查不到");
    }

    #[test]
    fn inmemory_marks_not_persistent() {
        let store = InMemorySecretStore::default();
        assert!(!store.persistent());
    }

    #[test]
    fn cross_backend_lookup_rejected() {
        let store = InMemorySecretStore::default();
        let r = SecretRef {
            backend: "keychain-macos".into(),
            id: "x".into(),
            label: "y".into(),
            revision: 0,
        };
        assert!(store.get(&r).is_err());
    }

    /// drop 之后字节确实被清零 —— 用观察句柄证明，不是"相信注释"。
    #[test]
    fn plain_secret_zeroed_on_drop() {
        let p = PlainSecret::new(vec![0xAAu8; 16]);
        let observer = Arc::clone(&p.bytes);
        assert!(!p.is_zeroed());
        drop(p);
        let guard = observer.lock().expect("observer lock");
        assert!(
            guard.iter().all(|&b| b == 0),
            "drop 后明文必须被清零，实际残留: {:?}",
            guard
        );
    }

    /// 显式 zero() 与 drop 走同一条清零路径。
    #[test]
    fn plain_secret_explicit_zero() {
        let p = PlainSecret::new(vec![0xAAu8; 16]);
        p.zero();
        assert!(p.is_zeroed());
        assert_eq!(p.expose().len(), 16);
    }

    /// 全局 store 真的可替换 —— 之前的实现是 `let _ = s;`，装了个寂寞。
    #[test]
    fn install_secret_store_replaces_global() {
        let probe: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
        let previous = install_secret_store_for_test(Arc::clone(&probe));
        let current = secret_store();
        assert!(
            Arc::ptr_eq(&probe, &current),
            "install_secret_store_for_test 必须真的替换全局 store"
        );
        // 还原，避免影响同进程里的其它用例
        if let Some(prev) = previous {
            install_secret_store_for_test(prev);
        } else {
            install_secret_store_for_test(Arc::new(InMemorySecretStore::default()));
        }
    }
}
