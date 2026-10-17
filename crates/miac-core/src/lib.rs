//! miac-core —— 米家空调控制核心（无界面、无进程副作用）
//!
//! 提供局域网与云端通信、型号能力、登录凭据、状态读取和后台工作线程。
//! 本 crate 不依赖界面库，由桌面端、CLI 和对话控制助手共用。

pub mod cloud;
pub mod controller;
pub mod credentials;
pub mod crypto;
pub mod dpapi;
pub mod login;
pub mod io_limits;
pub mod secret;
pub mod migration;
pub mod miio;
pub mod miot;
pub mod profile;
pub mod settings;
pub mod tls;
pub mod worker;
#[cfg(test)]
mod remediation_tests;

/// 通信通道策略，对应设置页的三选一。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// 自动：局域网优先，缺 localip/token 时改用云端
    Auto,
    /// 强制局域网直连
    Local,
    /// 强制云端 RPC
    Cloud,
}

impl Default for Transport {
    fn default() -> Self {
        Transport::Auto
    }
}

impl Transport {
    /// 界面分段按钮索引 → 通道（0 自动 1 局域网 2 云端）。
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Transport::Local,
            2 => Transport::Cloud,
            _ => Transport::Auto,
        }
    }

    /// 通道 → 界面分段按钮索引。
    pub fn to_index(self) -> i32 {
        match self {
            Transport::Auto => 0,
            Transport::Local => 1,
            Transport::Cloud => 2,
        }
    }

    /// 通道对应的实际链路名（用于状态栏显示）。
    pub fn label(self) -> &'static str {
        match self {
            Transport::Auto => "自动（局域网优先）",
            Transport::Local => "局域网直连",
            Transport::Cloud => "云端 RPC",
        }
    }
}
