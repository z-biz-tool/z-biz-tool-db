// T-050: SSH 隧道（评估与骨架）
//
// 本模块为 SSH 隧道功能提供架构骨架。完整实现需要评估并引入合适的 SSH crate。
//
// 架构要点：
// 1. 隧道生命周期绑定 connection revision
// 2. 只监听本机随机端口（安全：不暴露到外部网络）
// 3. 隧道断开后会话失效，不续接原事务
//
// 需要评估的 crate：
// - russh：纯 Rust 实现，更安全，但功能可能受限
// - ssh2：libssh2 绑定，功能完整，但需要 C 依赖
//
// 验收要求（A23）：
// - 隧道断开后会话失效
// - 不续接原事务
// - 隧道绑定 connection revision
// - 本机随机端口

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 隧道状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelState {
    /// 未建立
    Disconnected,
    /// 正在建立连接
    Connecting,
    /// 已建立，等待数据
    Connected,
    /// 本地端口已绑定
    Listening,
    /// 隧道关闭中
    Closing,
    /// 出错
    Error,
}

impl TunnelState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TunnelState::Disconnected => "disconnected",
            TunnelState::Connecting => "connecting",
            TunnelState::Connected => "connected",
            TunnelState::Listening => "listening",
            TunnelState::Closing => "closing",
            TunnelState::Error => "error",
        }
    }
}

/// SSH 隧道配置
#[derive(Debug, Clone)]
pub struct SshTunnelConfig {
    /// 远程主机
    pub remote_host: String,
    /// 远程端口（数据库端口）
    pub remote_port: u16,
    /// SSH 用户名
    pub ssh_user: String,
    /// SSH 主机
    pub ssh_host: String,
    /// SSH 端口（默认 22）
    pub ssh_port: u16,
    /// SSH 私钥路径（可选，使用系统 agent 时留空）
    pub private_key_path: Option<String>,
    /// 连接超时（秒）
    pub connect_timeout_secs: u64,
    /// 连接重试次数
    pub max_retries: u32,
}

impl Default for SshTunnelConfig {
    fn default() -> Self {
        Self {
            remote_host: "127.0.0.1".to_string(),
            remote_port: 5432,
            ssh_user: "root".to_string(),
            ssh_host: String::new(),
            ssh_port: 22,
            private_key_path: None,
            connect_timeout_secs: 30,
            max_retries: 3,
        }
    }
}

/// SSH 隧道实例
pub struct SshTunnel {
    config: SshTunnelConfig,
    state: Mutex<TunnelState>,
    /// 本地绑定的端口（动态分配）
    local_port: Mutex<Option<u16>>,
    /// 隧道创建时间
    created_at: Instant,
    /// 关联的 connection revision
    connection_revision: u32,
}

impl SshTunnel {
    /// 创建新的隧道实例
    pub fn new(config: SshTunnelConfig, connection_revision: u32) -> Self {
        Self {
            config,
            state: Mutex::new(TunnelState::Disconnected),
            local_port: Mutex::new(None),
            created_at: Instant::now(),
            connection_revision,
        }
    }

    /// 获取当前隧道状态
    pub fn state(&self) -> TunnelState {
        *self.state.lock().unwrap()
    }

    /// 获取本地端口
    pub fn local_port(&self) -> Option<u16> {
        *self.local_port.lock().unwrap()
    }

    /// 获取隧道运行时间
    pub fn uptime(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// 获取关联的 connection revision
    pub fn connection_revision(&self) -> u32 {
        self.connection_revision
    }

    /// 建立隧道（骨架实现）
    ///
    /// 实际实现需要：
    /// 1. 评估并引入合适的 SSH crate
    /// 2. 实现端口转发
    /// 3. 处理认证（密钥或密码）
    /// 4. 绑定本机随机端口
    pub async fn connect(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if *state != TunnelState::Disconnected {
            return Err("隧道已处于非断开状态".to_string());
        }
        *state = TunnelState::Connecting;

        // TODO: 实际 SSH 连接逻辑
        // - 需要引入 russh 或 ssh2 crate
        // - 配置连接参数
        // - 绑定本机随机端口
        // - 建立端口转发通道

        // 占位：连接成功后设置状态
        *state = TunnelState::Connected;
        Ok(())
    }

    /// 关闭隧道（骨架实现）
    pub async fn disconnect(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        *state = TunnelState::Closing;

        // TODO: 实际关闭逻辑
        // - 关闭 SSH 连接
        // - 释放本地端口

        *state = TunnelState::Disconnected;
        *self.local_port.lock().unwrap() = None;
        Ok(())
    }

    /// 获取隧道状态信息
    pub fn status(&self) -> TunnelStatus {
        TunnelStatus {
            state: self.state(),
            local_port: self.local_port(),
            uptime_secs: self.uptime().as_secs(),
            connection_revision: self.connection_revision,
        }
    }
}

/// 隧道状态信息（用于 IPC 返回）
#[derive(Debug, Clone, serde::Serialize)]
pub struct TunnelStatus {
    pub state: TunnelState,
    pub local_port: Option<u16>,
    pub uptime_secs: u64,
    pub connection_revision: u32,
}

/// 隧道管理器（进程内单例）
pub struct TunnelManager {
    tunnels: Mutex<Vec<SshTunnel>>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            tunnels: Mutex::new(Vec::new()),
        }
    }

    /// 注册新的隧道
    pub fn register(&self, tunnel: SshTunnel) {
        self.tunnels.lock().unwrap().push(tunnel);
    }

    /// 获取指定 connection revision 的隧道
    pub fn get_by_revision(&self, revision: u32) -> Option<SshTunnel> {
        self.tunnels
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.connection_revision() == revision)
            .cloned()
    }

    /// 移除并关闭指定隧道
    pub async fn remove(&self, revision: u32) -> Result<(), String> {
        let mut tunnels = self.tunnels.lock().unwrap();
        if let Some(pos) = tunnels.iter().position(|t| t.connection_revision() == revision) {
            let tunnel = tunnels.remove(pos);
            tunnel.disconnect().await?;
            Ok(())
        } else {
            Ok(())
        }
    }

    /// 获取所有活跃隧道
    pub fn active_tunnels(&self) -> Vec<TunnelStatus> {
        self.tunnels
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.state() != TunnelState::Disconnected)
            .map(|t| t.status())
            .collect()
    }

    /// 关闭所有隧道
    pub async fn close_all(&self) -> Result<(), String> {
        let tunnels: Vec<SshTunnel> = self.tunnels.lock().unwrap().drain(..).collect();
        for tunnel in tunnels {
            tunnel.disconnect().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_state_str_roundtrip() {
        assert_eq!(TunnelState::Connected.as_str(), "connected");
        assert_eq!(TunnelState::Disconnected.as_str(), "disconnected");
    }

    #[test]
    fn tunnel_config_defaults() {
        let config = SshTunnelConfig::default();
        assert_eq!(config.remote_port, 5432);
        assert_eq!(config.ssh_port, 22);
        assert_eq!(config.connect_timeout_secs, 30);
    }

    #[test]
    fn tunnel_manager_registration() {
        let manager = TunnelManager::new();
        let tunnel = SshTunnel::new(
            SshTunnelConfig::default(),
            1,
        );
        manager.register(tunnel);
        let status = manager.active_tunnels();
        assert!(status.is_empty(), "连接未建立时不应在活跃列表");
    }
}
