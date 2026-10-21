// T-014 接线：连接凭据的**封存（seal）**与**解封（unseal）**。
//
// 落盘不存明文密码：
// - seal：把明文交给 SecretStore，换回一个 SecretRef；落盘只写 SecretRef。
// - unseal：拿 SecretRef 换回明文；换不到就必须**显式**上报
//   （而不是让 password 静默变空串——用户分不清"我没填"还是"系统弄丢了"）。
//
// 这一层刻意不碰磁盘：它只做"记录 ⇄ 密钥库"的转换，存取盘仍由 queries.rs 负责。
// 好处是这两件事都能被纯函数式的用例覆盖，不需要真文件、不需要真数据库。

use crate::secrets::{PlainSecret, SecretRef, SecretStore};
use crate::ConnectionRecord;

/// 凭据状态：正常（明文已还原）
pub const CREDENTIAL_OK: &str = "ok";
/// 凭据状态：取不回来了（进程重启 / 密钥已被删 / 轮换过），必须重新输入
pub const CREDENTIAL_MISSING: &str = "missing";

/// 封存：把明文密码换进密钥库，返回**可落盘**的副本（明文已剥离）。
///
/// `existing` 是磁盘上已有的记录（含各自的 secret_ref）。它有两个用处：
/// 1. 明文没变时**复用**旧引用 —— 否则每次保存都会往密钥库里塞一条新副本，
///    旧副本永远没人删，明文在内存里越堆越多；
/// 2. 明文变了时把旧引用删掉，避免留下可用旧密码的悬挂凭据。
pub fn seal(
    store: &dyn SecretStore,
    incoming: &[ConnectionRecord],
    existing: &[ConnectionRecord],
) -> Result<Vec<ConnectionRecord>, String> {
    let mut out = Vec::with_capacity(incoming.len());
    for rec in incoming {
        let mut r = rec.clone();
        let old_ref: Option<SecretRef> = existing
            .iter()
            .find(|e| e.id == rec.id)
            .and_then(|e| e.secret_ref.clone());

        // 运行时状态不进磁盘
        r.credential_state = String::new();
        r.credential_note = None;

        if r.password.is_empty() {
            // 用户这轮没改密码：保留原引用（可能已是悬挂引用，
            // unseal 时会显式报 missing）
            r.secret_ref = old_ref;
            r.password = String::new();
            out.push(r);
            continue;
        }

        let label = if r.name.is_empty() {
            r.id.clone()
        } else {
            r.name.clone()
        };

        // 明文没变 ⇒ 复用旧引用，不产生新 secret
        if let Some(old) = &old_ref {
            if let Ok(current) = store.get(old) {
                if current.as_str() == r.password {
                    r.secret_ref = old_ref;
                    r.password = String::new();
                    out.push(r);
                    continue;
                }
            }
        }

        let plain = PlainSecret::new(r.password.clone());
        let new_ref = store
            .put(&label, &plain)
            .map_err(|e| format!("写入密钥库失败（连接 {}）: {}", label, e))?;
        // 旧 secret 被换掉了 ⇒ 删掉，不留可用旧密码的悬挂凭据
        if let Some(old) = &old_ref {
            if old.id != new_ref.id {
                store
                    .delete(old)
                    .map_err(|e| format!("清理旧密钥失败（连接 {}）: {}", label, e))?;
            }
        }
        r.secret_ref = Some(new_ref);
        r.password = String::new();
        out.push(r);
    }
    Ok(out)
}

/// 解封：把 SecretRef 换回明文；取不到的记录 `credential_state = "missing"`。
///
/// 返回 `(记录列表, 取不到密码的连接列表)`。调用方必须把第二项**展示给用户**——
/// 静默清空密码字段等于让用户以为是自己没填。
pub fn unseal(
    store: &dyn SecretStore,
    records: Vec<ConnectionRecord>,
) -> (Vec<ConnectionRecord>, Vec<MissingCredential>) {
    let mut out = Vec::with_capacity(records.len());
    let mut missing = Vec::new();
    for mut r in records {
        match r.secret_ref.clone() {
            None => {
                // 从来没有密钥引用：这条连接就没存过密码（用户本来就要自己填）
                r.password = String::new();
                r.credential_state = String::new();
                r.credential_note = None;
            }
            Some(sref) => match store.get(&sref) {
                Ok(plain) => {
                    r.password = plain.as_str();
                    r.credential_state = CREDENTIAL_OK.to_string();
                    r.credential_note = None;
                }
                Err(e) => {
                    r.password = String::new();
                    r.credential_state = CREDENTIAL_MISSING.to_string();
                    let reason = if store.persistent() {
                        format!("密钥库中找不到该连接的凭据（{}）：{}", r.name, e)
                    } else {
                        format!(
                            "连接 {} 的密码无法恢复（{}）：当前密钥库不跨进程持久，\
                             进程重启后密钥即被清空，请重新输入密码",
                            r.name, e
                        )
                    };
                    r.credential_note = Some(reason.clone());
                    missing.push(MissingCredential {
                        id: r.id.clone(),
                        name: r.name.clone(),
                        reason,
                    });
                }
            },
        }
        out.push(r);
    }
    (out, missing)
}

/// 取不到密码的连接（逐条列出，前端直接展示，别只给一个计数）
#[derive(Debug, Clone, serde::Serialize)]
pub struct MissingCredential {
    pub id: String,
    pub name: String,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::InMemorySecretStore;

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

    /// 落盘副本里绝不能出现明文
    #[test]
    fn seal_strips_plaintext_and_stores_ref() {
        let store = InMemorySecretStore::default();
        let incoming = vec![rec("c1", "生产库", "s3cr3t")];
        let sealed = seal(&store, &incoming, &[]).expect("seal");

        let json = serde_json::to_string(&sealed[0]).unwrap();
        assert!(
            !json.contains("s3cr3t"),
            "落盘副本里出现了明文密码: {}",
            json
        );
        assert!(sealed[0].password.is_empty());
        let sref = sealed[0].secret_ref.as_ref().expect("secret_ref");
        assert_eq!(sref.backend, "inmemory");
        // 明文确实进了密钥库
        assert_eq!(store.get(sref).expect("get").as_str(), "s3cr3t");
    }

    /// 落盘 → 读回，明文能还原
    #[test]
    fn unseal_restores_plaintext() {
        let store = InMemorySecretStore::default();
        let incoming = vec![rec("c1", "生产库", "s3cr3t")];
        let sealed = seal(&store, &incoming, &[]).expect("seal");
        let (unsealed, missing) = unseal(&store, sealed);
        assert!(missing.is_empty());
        assert_eq!(unsealed[0].password, "s3cr3t");
        assert_eq!(unsealed[0].credential_state, CREDENTIAL_OK);
    }

    /// 明文没变 ⇒ 复用旧引用，不往密钥库里堆新副本
    #[test]
    fn seal_reuses_ref_when_password_unchanged() {
        let store = InMemorySecretStore::default();
        let incoming = vec![rec("c1", "生产库", "s3cr3t")];
        let first = seal(&store, &incoming, &[]).expect("seal");
        let second = seal(&store, &incoming, &first).expect("seal");
        assert_eq!(
            first[0].secret_ref.as_ref().map(|r| r.id.clone()),
            second[0].secret_ref.as_ref().map(|r| r.id.clone()),
            "密码没变就不该产生新的 secret"
        );
    }

    /// 明文变了 ⇒ 换新引用并删掉旧的（不留可用旧密码的悬挂凭据）
    #[test]
    fn seal_rotates_ref_and_deletes_old() {
        let store = InMemorySecretStore::default();
        let incoming = vec![rec("c1", "生产库", "old-pass")];
        let first = seal(&store, &incoming, &[]).expect("seal");
        let old_ref = first[0].secret_ref.clone().expect("secret_ref");

        let mut changed = incoming.clone();
        changed[0].password = "new-pass".to_string();
        let second = seal(&store, &changed, &first).expect("seal");
        let new_ref = second[0].secret_ref.clone().expect("secret_ref");

        assert_ne!(old_ref.id, new_ref.id, "密码变了必须换引用");
        assert!(store.get(&old_ref).is_err(), "旧凭据必须被删掉");
        assert_eq!(store.get(&new_ref).expect("get").as_str(), "new-pass");
    }

    /// 密钥丢了 ⇒ 显式上报"需重新输入"，不静默清空
    #[test]
    fn unseal_reports_missing_credential_explicitly() {
        let store = InMemorySecretStore::default();
        let incoming = vec![rec("c1", "生产库", "s3cr3t")];
        let sealed = seal(&store, &incoming, &[]).expect("seal");
        // 模拟进程重启：换一个空的后端
        let fresh = InMemorySecretStore::default();
        let (unsealed, missing) = unseal(&fresh, sealed);
        assert!(unsealed[0].password.is_empty());
        assert_eq!(unsealed[0].credential_state, CREDENTIAL_MISSING);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "c1");
        assert!(
            missing[0].reason.contains("请重新输入密码"),
            "上报文案必须告诉用户要做什么: {}",
            missing[0].reason
        );
        assert!(unsealed[0].credential_note.is_some());
    }

    /// 从来没存过密码的连接不算"丢失"
    #[test]
    fn unseal_leaves_never_stored_record_alone() {
        let store = InMemorySecretStore::default();
        let (unsealed, missing) = unseal(&store, vec![rec("c1", "新连接", "")]);
        assert!(missing.is_empty());
        assert!(unsealed[0].credential_state.is_empty());
        assert!(unsealed[0].password.is_empty());
    }
}
