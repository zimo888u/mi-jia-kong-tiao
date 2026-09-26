//! worker.rs —— 控制核心的工作线程
//!
//! ## 为什么要有这一层
//!
//! `miio` 用阻塞 UDP、`cloud` 用阻塞 HTTPS，两者一次往返分别是 ~0.4 秒和
//! 1~2 秒。如果直接在 UI 线程上调用，界面会明显卡住（Slint 的 Timer 和
//! 重绘都跑在同一个线程上）。
//!
//! 但本项目又不打算引入 tokio——异步运行时本身会占掉数 MB 常驻内存，
//! 而验收指标是 25–45 MB。折中方案就是**一个后台线程 + 两个 channel**：
//!
//! ```text
//!   UI 线程 ──Command──▶ 工作线程（阻塞式 Controller）
//!          ◀──Event────
//! ```
//!
//! UI 侧用 `try_recv` 在定时器里非阻塞地取值，永远不阻塞重绘。
//!
//! ## 断线重连
//!
//! 工作线程内部维护「连续失败次数」。达到阈值就重新 `init_transport`，
//! 相当于自动重连；成功一次就清零。这样拔网线 / 路由器重启后不用手动刷新。

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::controller::{ActiveLink, Controller, ControllerError, PropValue, Snapshot};
use crate::credentials::Credentials;
use crate::settings::Settings;
use crate::Transport;

/// 界面发给工作线程的指令。
#[derive(Debug, Clone)]
pub enum Command {
    /// 初始化通道（内部会读 device.json 决定走局域网还是云端）
    Init,
    /// 重新选择通道（用户在设置页切换时）
    SetTransport(Transport),
    /// 读一次状态快照（含室温、电量、故障码）
    Snapshot,
    /// 读机器诊断
    Diag,
    /// 读温湿度计
    Thermometer,
    /// 读耗电日历
    PowerStats,
    /// 读自清洁/维护状态
    Maintenance,
    /// 写一个属性（按属性名）
    WriteProp { name: String, value: Value },
    /// 写一个属性（原始 siid/piid）
    RawWrite { siid: u16, piid: u16, value: Value },
    /// 读一个属性（原始 siid/piid）
    RawRead { siid: u16, piid: u16 },
    /// 应用已经开机后的待处理温度（会处理设备刚启动时的短暂拒绝）
    ApplyPreset(f64),
    /// 把凭据改成 DPAPI 加密存储
    EncryptCredentials,
    /// 退出线程
    Shutdown,
}

/// 工作线程回给界面的事件。
#[derive(Debug, Clone)]
pub enum Event {
    /// 通道已就绪
    Ready { link: ActiveLink, device: DeviceSummary },
    /// 通道初始化失败（通常是没登录过）
    NotReady { reason: String },
    /// 状态快照
    Snapshot(Box<Snapshot>),
    /// 机器诊断
    Diag(Vec<(&'static str, PropValue)>),
    /// 温湿度计
    Thermometer(crate::controller::ThermometerReading),
    /// 耗电日历
    PowerStats(Box<crate::controller::PowerStats>),
    /// 自清洁/维护
    Maintenance { values: Vec<(&'static str, PropValue)>, cleaning: bool },
    /// 原始属性读取结果
    RawValue { siid: u16, piid: u16, value: PropValue },
    /// 写操作完成
    Wrote { name: String, ok: bool, error: Option<String> },
    /// 一次操作失败（界面用提示条显示）
    Failed { op: String, error: String },
    /// DPAPI 凭据加密任务完成。只有所有现有凭据均成功写入密文才算成功。
    CredentialsEncrypted { ok: bool, error: Option<String> },
    /// 日志
    Log(String),
}

/// 设备摘要（界面「设置」页展示，不含 token）。
#[derive(Debug, Clone, Default)]
pub struct DeviceSummary {
    pub profile: crate::profile::Profile,
    pub name: String,
    pub model: String,
    pub did: String,
    pub localip: Option<String>,
    pub has_token: bool,
    pub saved_at: Option<String>,
    pub model_matches: bool,
    pub credentials_dir: String,
    pub has_cloud_session: bool,
    pub has_thermometer: bool,
}

/// 工作线程句柄：界面拿它发指令、收事件。
pub struct Worker {
    tx: Sender<Command>,
    rx: Receiver<Event>,
    handle: std::cell::RefCell<Option<thread::JoinHandle<()>>>,
}

impl Worker {
    /// 起一个工作线程。
    pub fn spawn(creds: Credentials, settings: Settings) -> Self {
        let (cmd_tx, cmd_rx) = channel::<Command>();
        let (evt_tx, evt_rx) = channel::<Event>();

        let handle = thread::Builder::new()
            .name("miac-worker".into())
            // 栈开小一点：这个线程只跑同步网络调用，不需要 8 MB 默认栈
            .stack_size(512 * 1024)
            .spawn(move || {
                // 后台线程 panic 时如果不打印，界面只会一直停在「连接中…」，
                // 排查时毫无线索（本项目就踩过：工作线程在握手时 panic，
                // 事件通道随之断开，界面既不报错也不更新）。这里显式接住并打印。
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut w = WorkerLoop::new(creds, settings, evt_tx.clone());
                    w.run(cmd_rx);
                }));
                if let Err(e) = result {
                    let msg = if let Some(s) = e.downcast_ref::<&str>() {
                        (*s).to_string()
                    } else if let Some(s) = e.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "未知 panic".to_string()
                    };
                    eprintln!("[工作线程] 已崩溃：{msg}");
                    let _ = evt_tx.send(Event::Log(format!("[工作线程] 已崩溃：{msg}")));
                }
            })
            .expect("无法启动工作线程");

        Self { tx: cmd_tx, rx: evt_rx, handle: std::cell::RefCell::new(Some(handle)) }
    }

    /// 发指令（忽略发送失败——线程已退出时说明程序正在关闭）。
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// 非阻塞取一个事件。界面定时器里调用，绝不阻塞重绘。
    pub fn try_recv(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(e) => Some(e),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    /// 一次取走所有待处理事件（避免定时器跟不上事件产生速度时积压）。
    pub fn drain(&self, out: &mut Vec<Event>) {
        while let Some(e) = self.try_recv() {
            out.push(e);
            // 一次最多取 64 条，防止极端情况下界面被卡在一个 tick 里
            if out.len() >= 64 {
                break;
            }
        }
    }

    /// 通知线程退出并等待它收尾。
    ///
    /// 用 `&self` + `RefCell` 而不是 `&mut self`：
    /// 界面侧把 Worker 包在 `Rc` 里共享给多个回调，`Rc` 拿不到 `&mut`。
    /// 句柄本来就只在关闭时取一次，放进 RefCell 足够。
    pub fn shutdown(&self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(h) = self.handle.borrow_mut().take() {
            let _ = h.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 连续失败多少次就触发重连。
const RECONNECT_AFTER_FAILURES: u32 = 3;

/// 工作线程主体。
struct WorkerLoop {
    creds: Credentials,
    settings: Settings,
    tx: Sender<Event>,
    ctrl: Option<Controller>,
    /// 连续失败次数（成功一次清零）
    failures: u32,
    /// 上次重连尝试时间，避免疯狂重连
    last_reconnect: Instant,
}

impl WorkerLoop {
    fn new(creds: Credentials, settings: Settings, tx: Sender<Event>) -> Self {
        Self {
            creds,
            settings,
            tx,
            ctrl: None,
            failures: 0,
            last_reconnect: Instant::now() - Duration::from_secs(60),
        }
    }

    fn log(&self, msg: impl Into<String>) {
        let msg = msg.into();
        // 同时打到 stderr：界面日志区在窗口里，排查启动期问题时看不到，
        // 而无头运行（--probe / --self-test）时更没有界面。
        eprintln!("[工作线程] {msg}");
        let _ = self.tx.send(Event::Log(msg));
    }

    fn run(&mut self, rx: Receiver<Command>) {
        self.log("已启动");
        loop {
            // 带超时的 recv：没有指令时定期做一次「自愈」（检查是否需要重连）
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Command::Shutdown) => {
                    self.log("收到退出指令");
                    if let Some(c) = self.ctrl.as_mut() {
                        c.dispose();
                    }
                    break;
                }
                Ok(cmd) => self.handle(cmd),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    self.maybe_reconnect();
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.log("指令通道已断开，退出");
                    break;
                }
            }
        }
    }

    /// 断线自愈：连续失败到阈值后重新初始化通道。
    fn maybe_reconnect(&mut self) {
        if self.failures < RECONNECT_AFTER_FAILURES {
            return;
        }
        // 距上次重连至少 10 秒，避免设备离线时疯转
        if self.last_reconnect.elapsed() < Duration::from_secs(10) {
            return;
        }
        self.last_reconnect = Instant::now();
        self.log(format!(
            "[重连] 连续失败 {} 次，尝试重新建立通道…",
            self.failures
        ));
        // 丢掉旧连接，强制重新握手
        if let Some(c) = self.ctrl.as_mut() {
            c.dispose();
        }
        self.ctrl = None;
        match self.ensure() {
            Ok(link) => {
                self.failures = 0;
                self.log(format!("[重连] 成功，当前通道：{}", link.label()));
                let _ = self.tx.send(Event::Ready {
                    link,
                    device: self.device_summary(),
                });
            }
            Err(e) => {
                self.log(format!("[重连] 失败：{e}"));
            }
        }
    }

    /// 确保控制器已初始化，返回当前通道。
    fn ensure(&mut self) -> Result<ActiveLink, ControllerError> {
        if let Some(c) = self.ctrl.as_ref() {
            if let Some(link) = c.link {
                return Ok(link);
            }
        }
        let mut c = Controller::new(self.creds.clone(), self.settings.transport());
        let link = c.init_transport()?;
        self.ctrl = Some(c);
        Ok(link)
    }

    /// 设备信息摘要。
    fn device_summary(&self) -> DeviceSummary {
        let dir = self.creds.primary_dir().display().to_string();
        let has_cloud_session = self.creds.read_session().is_some_and(|s| s.ready());
        let has_thermometer = self
            .creds
            .read_thermometer()
            .is_some_and(|t| !t.did.trim().is_empty());

        match self.creds.read_device() {
            Some(d) => DeviceSummary {
                profile: self.ctrl.as_ref().map(|c| c.profile.clone()).unwrap_or_default(),
                name: d.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| d.model.clone().unwrap_or_else(|| "米家空调".into())),
                model: d.model.clone().unwrap_or_default(),
                did: d.did.clone(),
                localip: d.localip.clone(),
                has_token: d.token.as_deref().is_some_and(|t| !t.trim().is_empty()),
                saved_at: d.saved_at.clone(),
                model_matches: self.ctrl.as_ref().is_some_and(|c| c.profile.writable("on")),
                credentials_dir: dir,
                has_cloud_session,
                has_thermometer,
            },
            None => DeviceSummary {
                credentials_dir: dir,
                has_cloud_session,
                has_thermometer,
                ..Default::default()
            },
        }
    }

    /// 记录一次成功/失败，用于断线重连判定。
    fn note(&mut self, result: &Result<(), ControllerError>) {
        match result {
            Ok(()) => self.failures = 0,
            Err(_) => self.failures = self.failures.saturating_add(1),
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Shutdown => unreachable!("run() 里已单独处理"),

            Command::Init | Command::SetTransport(_) => {
                if let Command::SetTransport(t) = cmd {
                    self.settings.transport = t.to_index();
                }
                // Init also follows a newly selected device. Reusing the old
                // controller would report the new name while controlling the old DID.
                if let Some(c) = self.ctrl.as_mut() {
                    c.dispose();
                }
                self.ctrl = None;
                self.failures = 0;
                match self.ensure() {
                    Ok(link) => {
                        self.failures = 0;
                        let _ = self.tx.send(Event::Ready {
                            link,
                            device: self.device_summary(),
                        });
                    }
                    Err(e) => {
                        let _ = self.tx.send(Event::NotReady { reason: e.to_string() });
                    }
                }
            }

            Command::Snapshot => {
                let r = self.ensure().and_then(|_| {
                    self.ctrl.as_mut().expect("ensure 后一定有控制器").snapshot()
                });
                match r {
                    Ok(s) => {
                        self.failures = 0;
                        let _ = self.tx.send(Event::Snapshot(Box::new(s)));
                    }
                    Err(e) => {
                        self.note(&Err(e.clone()));
                        let _ = self.tx.send(Event::Failed { op: "读状态".into(), error: e.to_string() });
                    }
                }
            }

            Command::Diag => {
                let r = self
                    .ensure()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").diag());
                match r {
                    Ok(v) => {
                        self.failures = 0;
                        let _ = self.tx.send(Event::Diag(v));
                    }
                    Err(e) => {
                        self.note(&Err(e.clone()));
                        let _ = self
                            .tx
                            .send(Event::Failed { op: "读诊断".into(), error: e.to_string() });
                    }
                }
            }

            Command::Thermometer => {
                // 温湿度计失败不影响空调控制，所以不参与重连计数
                if self.ensure().is_ok() {
                    let t = self.ctrl.as_mut().expect("有控制器").read_thermometer();
                    let _ = self.tx.send(Event::Thermometer(t));
                }
            }

            Command::PowerStats => {
                let r = self
                    .ensure()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").power_stats());
                match r {
                    Ok(p) => {
                        self.failures = 0;
                        let _ = self.tx.send(Event::PowerStats(Box::new(p)));
                    }
                    Err(e) => {
                        let _ = self.tx.send(Event::Failed {
                            op: "读电量统计".into(),
                            error: e.to_string(),
                        });
                    }
                }
            }

            Command::Maintenance => {
                let r = self
                    .ensure()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").maintenance());
                match r {
                    Ok((values, cleaning)) => {
                        let _ = self.tx.send(Event::Maintenance { values, cleaning });
                    }
                    Err(e) => {
                        let _ = self.tx.send(Event::Failed {
                            op: "读维护状态".into(),
                            error: e.to_string(),
                        });
                    }
                }
            }

            Command::WriteProp { name, value } => {
                let r = self
                    .ensure()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").write_prop(&name, value));
                let ok = r.is_ok();
                // A rejected value or a device business error is not evidence
                // that the transport is down. Reads drive reconnection counts.
                let _ = self.tx.send(Event::Wrote {
                    name,
                    ok,
                    error: r.err().map(|e| e.to_string()),
                });
            }

            Command::RawWrite { siid, piid, value } => {
                let r = self
                    .ensure()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").raw_write(siid, piid, value));
                match r {
                    Ok(v) => {
                        let _ = self.tx.send(Event::RawValue { siid, piid, value: v });
                    }
                    Err(e) => {
                        let _ = self.tx.send(Event::Failed {
                            op: format!("写 {siid}.{piid}"),
                            error: e.to_string(),
                        });
                    }
                }
            }

            Command::RawRead { siid, piid } => {
                let r = self
                    .ensure()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").raw_read(siid, piid));
                match r {
                    Ok(v) => {
                        let _ = self.tx.send(Event::RawValue { siid, piid, value: v });
                    }
                    Err(e) => {
                        let _ = self.tx.send(Event::Failed {
                            op: format!("读 {siid}.{piid}"),
                            error: e.to_string(),
                        });
                    }
                }
            }

            Command::ApplyPreset(t) => {
                let r = self
                    .ensure()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").apply_temp_preset(t));
                match r {
                    Ok((turned_on, temp)) => {
                        self.failures = 0;
                        let msg = if turned_on {
                            format!("[预设] 已开机并设为 {temp:.1} ℃")
                        } else {
                            format!("[预设] 已设为 {temp:.1} ℃")
                        };
                        self.log(msg);
                        let _ = self.tx.send(Event::Wrote {
                            name: "targetTemp".into(),
                            ok: true,
                            error: None,
                        });
                    }
                    Err(e) => {
                        self.note(&Err(e.clone()));
                        let _ = self.tx.send(Event::Failed {
                            op: "应用温度预设".into(),
                            error: e.to_string(),
                        });
                    }
                }
            }

            Command::EncryptCredentials => {
                self.log("[凭据] 开始改用 DPAPI 加密存储…");
                let mut done = 0;
                let mut failed = Vec::new();

                // device.json
                if let Some(d) = self.creds.read_device() {
                    match self.creds.write_json_maybe_encrypted(
                        crate::credentials::FILE_DEVICE,
                        &d,
                        true,
                    ) {
                        Ok(_) => done += 1,
                        Err(e) => failed.push(format!("device.json: {e}")),
                    }
                }
                // cloud-session.json
                if let Some(s) = self.creds.read_session() {
                    match self.creds.write_json_maybe_encrypted(
                        crate::credentials::FILE_SESSION,
                        &s,
                        true,
                    ) {
                        Ok(_) => done += 1,
                        Err(e) => failed.push(format!("cloud-session.json: {e}")),
                    }
                }
                // thermometer.json
                if let Some(t) = self.creds.read_thermometer() {
                    match self.creds.write_json_maybe_encrypted(
                        crate::credentials::FILE_THERMOMETER,
                        &t,
                        true,
                    ) {
                        Ok(_) => done += 1,
                        Err(e) => failed.push(format!("thermometer.json: {e}")),
                    }
                }

                if failed.is_empty() {
                    self.log(format!("[凭据] 已加密 {done} 个文件（绑定当前 Windows 用户与本机）"));
                    let _ = self.tx.send(Event::CredentialsEncrypted { ok: true, error: None });
                } else {
                    let error = failed.join("；");
                    self.log(format!("[凭据] 加密失败：{error}"));
                    let _ = self.tx.send(Event::CredentialsEncrypted { ok: false, error: Some(error) });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_discards_old_device_connection_after_credentials_change() {
        let creds = Credentials::isolated(std::env::temp_dir().join("miac-no-device-for-init-test"));
        let (tx, rx) = channel();
        let mut loop_state = WorkerLoop::new(creds.clone(), Settings::default(), tx);
        let mut old_controller = Controller::new(creds, Transport::Auto);
        old_controller.link = Some(ActiveLink::Local);
        loop_state.ctrl = Some(old_controller);

        loop_state.handle(Command::Init);

        assert!(loop_state.ctrl.is_none());
        assert!(matches!(rx.try_recv(), Ok(Event::NotReady { .. })));
    }

    #[test]
    fn worker_starts_and_stops() {
        // 用一个空目录当凭据目录，并用 isolated 切断回退查找，
        // 保证初始化必然失败（没有 device.json），且不会读到本机真实凭据。
        let dir = std::env::temp_dir().join("miac-test-worker-empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let creds = Credentials::isolated(&dir);
        let w = Worker::spawn(creds, Settings::default());

        w.send(Command::Init);
        // 等一小会儿让线程处理
        std::thread::sleep(Duration::from_millis(300));

        let mut events = Vec::new();
        w.drain(&mut events);

        // 至少要有一条 NotReady（没有 device.json）
        assert!(
            events.iter().any(|e| matches!(e, Event::NotReady { .. })),
            "空凭据目录下应该报 NotReady，实际事件：{events:?}"
        );

        w.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn try_recv_is_non_blocking() {
        let dir = std::env::temp_dir().join("miac-test-worker-nonblock");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let creds = Credentials::isolated(&dir);
        let w = Worker::spawn(creds, Settings::default());

        // 没有指令时不阻塞，立刻返回 None
        let start = Instant::now();
        assert!(w.try_recv().is_none());
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "try_recv 不应阻塞"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
