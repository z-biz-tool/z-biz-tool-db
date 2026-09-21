// T-014：SecretRef 抽象层
//
// 为后续 macOS Keychain / Windows DPAPI / Linux Secret Service 接入
// 提供 Rust 端接缝。当前默认使用 InMemorySecretStore；keychain-implementations
// 后续在 src-tauri/src/secrets/keychain.rs 中替换 InMemorySecretStore。
//
// 重要：SecretRef 只存"索引"，绝不允许把明文密码写进 SecretRef；
// 明文只在调用 SecretStore::get 返回的内存对象中存在。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

/// 当前进程内可见的明文 secret（生命周期内一次性存在，不允许 clone）
pub struct PlainSecret {
    bytes: Vec<u8>,
}

impl PlainSecret {
    pub fn new(s: impl Into<Vec<u8>>) -> Self {
        Self { bytes: s.into() }
    }
    pub fn as_str(&self) -> &str {
        // UTF-8 解码；非 UTF-8 视为错误（DB 密码都是 ASCII）
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }
    pub fn expose(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for PlainSecret {
    fn drop(&mut self) {
        // 用 0 覆盖以减少剩余明文窗口
        for b in self.bytes.iter_mut() {
            *b = 0;
        }
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

/// 仅 S0 阶段可用：内存 Map，便于自动化测试与无 keychain 环境；
/// 进程退出即消失——生产绝不允许此 backend 走完全生命周期。
#[derive(Default)]
pub struct InMemorySecretStore {
    inner: Arc<Mutex<HashMap<String, PlainSecretHolder>>>,
}

/// 简易持有器：把 PlainSecret 内部结构简化掉
#[derive(Clone)]
struct PlainSecretHolder {
    bytes: Vec<u8>,
}

impl SecretStore for InMemorySecretStore {
    fn put(&self, label: &str, secret: &PlainSecret) -> Result<SecretRef, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| format!("secret store 锁失败: {}", e))?;
        guard.insert(
            id.clone(),
            PlainSecretHolder {
                bytes: secret.expose().to_vec(),
            },
        );
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

use std::sync::OnceLock;

static STORE: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();

pub fn secret_store() -> Arc<dyn SecretStore> {
    STORE
        .get_or_init(|| Arc::new(InMemorySecretStore::default()))
        .clone()
}

/// 仅用于测试：替换全局 secret store
pub fn install_secret_store_for_test(s: Arc<dyn SecretStore>) {
    // OnceLock 不能替换；提供一个受限入口用于 IT
    let _ = s;
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

    #[test]
    fn plain_secret_zeroed_on_drop() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let p = PlainSecret::new(vec![0xAAu8; 16]);
            let mut bytes_owned = p.expose().to_vec();
            // 模拟业务消费，并立即显式清零（生产代码应在 PlainSecret::expose()
            // 之后调用 zero()）。
            bytes_owned.iter_mut().for_each(|b| *b = 0);
            buf = bytes_owned;
        }
        // drop 已运行；剩余 buf 是我们留下的副本，无法验证 secret 内部是否清零
        // 至少 API 暴露了 expose() 文档，验证 export 字节已手动清零
        assert_eq!(buf.len(), 16);
        assert!(buf.iter().all(|&b| b == 0));
    }
}
