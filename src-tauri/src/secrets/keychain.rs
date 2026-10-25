// T-014 续：OS Keychain 后端 —— macOS Keychain / Windows Credential Manager /
// Linux Secret Service（由 `keyring` crate 按平台自动选择）。
//
// 设计要点：
// - 接口完全复用 secrets.rs 的 `SecretStore` trait，**零签名变动**；
// - 后端可配置：env `ZBIZ_SECRET_BACKEND = keychain | inmemory`，缺省 `auto`
//   （能开 keychain 就用，开不了回退 inmemory 并打 warning，绝不静默）；
// - 命名规范：service = 应用名 `z-biz-tool-db`，account = SecretRef.id（UUID v4）。
//   测试一律用 `z-biz-tool-db-test` service，绝不碰真实条目。

use std::sync::Arc;

use keyring::Entry;

use super::{InMemorySecretStore, PlainSecret, SecretRef, SecretStore};

/// 生产环境写入 OS 密钥库时使用的 service 名。
pub const KEYCHAIN_SERVICE: &str = "z-biz-tool-db";

/// 测试专用 service 名：与真实 service 物理隔离，CI 里误跑也不会污染用户凭据。
pub const TEST_KEYCHAIN_SERVICE: &str = "z-biz-tool-db-test";

/// 选择 secret 后端的 env var。
pub const ENV_SECRET_BACKEND: &str = "ZBIZ_SECRET_BACKEND";

/// 可用性探测用的假 account（只读不存在的条目，不会创建任何东西）。
const AVAILABILITY_PROBE_ACCOUNT: &str = "__availability_probe__";

/// SecretRef.backend 字段在本平台下的取值（与 secrets.rs 文档注释对齐）。
pub fn backend_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "keychain-macos"
    } else if cfg!(target_os = "windows") {
        "keychain-windows"
    } else {
        "secret-service"
    }
}

/// 基于 OS 密钥库的 SecretStore 实现。
pub struct KeychainSecretStore {
    service: String,
}

impl KeychainSecretStore {
    /// 用指定 service 建后端（测试传 TEST_KEYCHAIN_SERVICE）。
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// 生产后端：service = "z-biz-tool-db"。
    pub fn production() -> Self {
        Self::new(KEYCHAIN_SERVICE)
    }

    fn entry(&self, account: &str) -> Result<Entry, String> {
        Entry::new(&self.service, account)
            .map_err(|e| format!("创建密钥库条目失败（service={}）: {}", self.service, e))
    }

    /// 探测密钥库是否真的可用：后端已初始化 + 能完成一次读取。
    ///
    /// 探测读的是一个**不存在**的条目，不会创建、不会弹窗、不碰真实数据。
    pub fn check_available(&self) -> Result<(), String> {
        if let Err(e) = Entry::store_status() {
            return Err(format!("密钥库后端未就绪: {}", e));
        }
        let probe = self.entry(AVAILABILITY_PROBE_ACCOUNT)?;
        match probe.get_secret() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("密钥库不可访问: {}", e)),
        }
    }
}

impl SecretStore for KeychainSecretStore {
    fn put(&self, label: &str, secret: &PlainSecret) -> Result<SecretRef, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let entry = self.entry(&id)?;
        entry
            .set_secret(&secret.expose())
            .map_err(|e| format!("写入密钥库失败（account={}）: {}", id, e))?;
        Ok(SecretRef {
            backend: backend_name().to_string(),
            id,
            label: label.to_string(),
            revision: 0,
        })
    }

    fn get(&self, secret_ref: &SecretRef) -> Result<PlainSecret, String> {
        // 与 InMemorySecretStore 一致：不跨后端取。旧版（backend=="inmemory"）
        // 引用会走到这里并**显式报错**，由 unseal 标成 missing，不静默失败。
        if secret_ref.backend != backend_name() {
            return Err(format!(
                "keychain 后端不支持其它后端的引用: {}",
                secret_ref.backend
            ));
        }
        let entry = self.entry(&secret_ref.id)?;
        match entry.get_secret() {
            Ok(bytes) => Ok(PlainSecret::new(bytes)),
            Err(keyring::Error::NoEntry) => {
                Err(format!("密钥库中找不到条目（account={}）", secret_ref.id))
            }
            Err(e) => Err(format!("读取密钥库失败: {}", e)),
        }
    }

    fn delete(&self, secret_ref: &SecretRef) -> Result<(), String> {
        // 幂等且容忍历史引用：seal 在从 inmemory 迁到 keychain 时会拿着
        // 旧后端引用来 delete，这里必须放行（本后端没有这条 = 删成功），
        // 否则保存连接会直接失败。
        if secret_ref.backend != backend_name() {
            return Ok(());
        }
        let entry = self.entry(&secret_ref.id)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("删除密钥库条目失败: {}", e)),
        }
    }

    fn persistent(&self) -> bool {
        true
    }
}

/// 读 env，装配后端（纯函数：env 值作参数，便于测试，不碰全局状态）。
///
/// - `inmemory`：强制内存后端（CI / 无 keychain 环境用）；
/// - `keychain`：强制 keychain，不可用则回退 inmemory 并打 warning；
/// - `auto` / 空 / 无法识别的值：能开 keychain 就用，否则回退 + warning。
pub fn select_backend_from(env_value: Option<&str>) -> Arc<dyn SecretStore> {
    let normalized = env_value.map(|v| v.trim().to_ascii_lowercase());
    match normalized.as_deref() {
        Some("inmemory") => {
            eprintln!(
                "[warn] {}=inmemory：密码仅进程内有效，进程重启后需重新输入",
                ENV_SECRET_BACKEND
            );
            Arc::new(InMemorySecretStore::default())
        }
        Some("keychain") => match try_keychain() {
            Ok(store) => Arc::new(store),
            Err(e) => {
                eprintln!(
                    "[warn] {}=keychain 但 keychain 不可用（{}），已回退 inmemory：密码将不跨进程持久",
                    ENV_SECRET_BACKEND, e
                );
                Arc::new(InMemorySecretStore::default())
            }
        },
        // auto / 空：默认走 keychain，失败回退
        None | Some("") | Some("auto") => match try_keychain() {
            Ok(store) => Arc::new(store),
            Err(e) => {
                eprintln!(
                    "[warn] keychain 不可用（{}），回退 inmemory：密码仅进程内有效，进程重启后需重新输入",
                    e
                );
                Arc::new(InMemorySecretStore::default())
            }
        },
        // 无法识别的值：警告后按 auto 处理（不静默忽略用户的配置意图）
        other => {
            eprintln!(
                "[warn] {} 取值无法识别（{:?}），按 auto 处理",
                ENV_SECRET_BACKEND,
                other.unwrap_or_default()
            );
            match try_keychain() {
                Ok(store) => Arc::new(store),
                Err(e) => {
                    eprintln!(
                        "[warn] keychain 不可用（{}），回退 inmemory：密码仅进程内有效，进程重启后需重新输入",
                        e
                    );
                    Arc::new(InMemorySecretStore::default())
                }
            }
        }
    }
}

/// 按进程 env 装配后端（`secret_store()` 的默认装配入口）。
pub fn select_backend() -> Arc<dyn SecretStore> {
    select_backend_from(std::env::var(ENV_SECRET_BACKEND).ok().as_deref())
}

fn try_keychain() -> Result<KeychainSecretStore, String> {
    let store = KeychainSecretStore::production();
    store.check_available()?;
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::credentials::{seal, unseal, CREDENTIAL_MISSING};
    use crate::ConnectionRecord;

    fn rec(id: &str, name: &str, password: &str) -> ConnectionRecord {
        ConnectionRecord {
            id: id.to_string(),
            name: name.to_string(),
            db_type: "mysql".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3306,
            username: "root".to_string(),
            password: password.to_string(),
            database: "app".to_string(),
            revision: 1,
            environment: "dev".to_string(),
            tls_mode: None,
            tls_ca_path: None,
            secret_ref: None,
            credential_state: String::new(),
            credential_note: None,
        }
    }

    /// keychain 不可用时优雅跳过：打印 skip 原因，**不算失败**。
    fn keychain_or_skip() -> Option<KeychainSecretStore> {
        let store = KeychainSecretStore::new(TEST_KEYCHAIN_SERVICE);
        match store.check_available() {
            Ok(()) => Some(store),
            Err(e) => {
                eprintln!("[skip] keychain 后端不可用，跳过 OS 密钥库实测: {}", e);
                None
            }
        }
    }

    /// put / get / delete 直走后端的 roundtrip。
    #[test]
    fn put_get_delete_roundtrip_on_keychain() {
        let Some(store) = keychain_or_skip() else {
            return;
        };
        let plain = PlainSecret::new("k3y-ch4in-pass");
        let r = store.put("测试连接", &plain).expect("put");
        assert_eq!(r.backend, backend_name(), "ref 必须带本平台后端名");

        let got = store.get(&r).expect("get");
        assert_eq!(got.as_str(), "k3y-ch4in-pass");

        store.delete(&r).expect("delete");
        assert!(store.get(&r).is_err(), "删除后应取不到");
    }

    /// seal / unseal 走 keychain 后端 roundtrip：
    /// 读回时换一个**全新实例**，证明数据来自 OS 密钥库而不是进程内 map。
    /// 用测试专用 service 名，测后清理条目，不污染真实 "z-biz-tool-db"。
    #[test]
    fn seal_unseal_roundtrip_with_keychain_backend() {
        let Some(store) = keychain_or_skip() else {
            return;
        };
        let incoming = vec![rec("kc-1", "密钥库连接", "roundtrip-p@ss")];
        let sealed = seal(&store, &incoming, &[]).expect("seal");
        assert_eq!(
            sealed[0].secret_ref.as_ref().map(|r| r.backend.clone()),
            Some(backend_name().to_string())
        );

        let fresh = KeychainSecretStore::new(TEST_KEYCHAIN_SERVICE);
        let (unsealed, missing) = unseal(&fresh, sealed.clone());
        assert!(
            missing.is_empty(),
            "keychain 里的凭据必须能取回: {:?}",
            missing
        );
        assert_eq!(unsealed[0].password, "roundtrip-p@ss");
        assert_eq!(unsealed[0].credential_state, "ok");

        // 清理测试条目
        if let Some(r) = sealed[0].secret_ref.as_ref() {
            store.delete(r).expect("清理测试 keychain 条目");
        }
        // 删完后必须真的取不到 —— 证明 unseal 读的是 OS 密钥库，不是进程内 map
        let (_, missing_after) = unseal(&fresh, sealed);
        assert_eq!(
            missing_after.len(),
            1,
            "删除后必须取不到，否则说明有进程内缓存"
        );
    }

    /// 旧版 backend=="inmemory" 的磁盘引用：secret 早已丢失（inmemory 从不
    /// 持久化）。必须**显式**报 missing，不静默清空；delete 必须幂等放行，
    /// 否则用户下次保存连接时 seal 清理旧引用会直接失败。
    #[test]
    fn legacy_inmemory_ref_marked_missing_and_delete_tolerated() {
        // 此用例不碰真 keychain：get 在 backend 校验处就返回 Err，delete 直接 Ok。
        let store = KeychainSecretStore::new(TEST_KEYCHAIN_SERVICE);
        let legacy = SecretRef::new_inmemory("old-uuid", "旧连接");
        let mut r = rec("legacy-1", "旧连接", "");
        r.secret_ref = Some(legacy.clone());

        let (unsealed, missing) = unseal(&store, vec![r]);
        assert_eq!(unsealed[0].credential_state, CREDENTIAL_MISSING);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "legacy-1");
        assert!(
            missing[0].reason.contains("请重新输入密码"),
            "旧 inmemory 引用必须提示重新输入: {}",
            missing[0].reason
        );
        store.delete(&legacy).expect("删除历史后端引用必须幂等成功");
    }

    /// PlainSecret 从 keychain 取回后，drop 仍然清零（用观察句柄证伪）。
    #[test]
    fn keychain_secret_zeroed_on_drop() {
        let Some(store) = keychain_or_skip() else {
            return;
        };
        let plain = PlainSecret::new("zero-me-please");
        let r = store.put("清零用例", &plain).expect("put");
        let got = store.get(&r).expect("get");
        let observer = Arc::clone(&got.bytes);
        assert!(!got.is_zeroed());
        drop(got);
        let guard = observer.lock().expect("observer lock");
        assert!(
            guard.iter().all(|&b| b == 0),
            "drop 后明文必须被清零，实际残留: {:?}",
            guard
        );
        store.delete(&r).expect("清理");
    }

    /// 后端选择：env 语义（纯函数，不动全局 env，不写任何密钥库）。
    #[test]
    fn select_backend_honours_env() {
        assert!(
            !select_backend_from(Some("inmemory")).persistent(),
            "显式 inmemory 必须给非持久后端"
        );
        assert!(
            !select_backend_from(Some(" INMEMORY ")).persistent(),
            "大小写与空白应被归一化"
        );
    }

    /// auto / keychain：本机 keychain 可用时必须选到持久后端；
    /// 不可用则走回退分支（不 panic）。探测只读，不写真实 service。
    #[test]
    fn select_backend_auto_prefers_keychain_when_available() {
        let available = KeychainSecretStore::production().check_available().is_ok();
        for env in [None, Some("auto"), Some("keychain"), Some("bogus-value")] {
            let store = select_backend_from(env);
            if available {
                assert!(
                    store.persistent(),
                    "env={:?} 在 keychain 可用时应选持久后端",
                    env
                );
            } else {
                eprintln!("[skip] keychain 不可用，回退分支已被覆盖（env={:?}）", env);
            }
        }
    }
}
