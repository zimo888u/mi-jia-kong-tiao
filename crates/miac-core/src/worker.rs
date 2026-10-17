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
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
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

struct Envelope {
    command: Command,
    connection_id: Option<u64>,
}

fn is_device_write(command: &Command) -> bool {
    matches!(command, Command::WriteProp { .. } | Command::RawWrite { .. } | Command::ApplyPreset(_))
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
    /// 云端票据过期：每次登录周期只提示一次，不影响局域网控制。
    CloudLoginRequired,
    /// DPAPI 凭据加密任务完成。只有所有现有凭据均成功写入密文才算成功。
    CredentialsEncrypted { ok: bool, error: Option<String> },
    /// 日志
    Log(String),
    Stopped { reason: String },
}

/// 设备摘要（界面「设置」页展示，不含 token）。
#[derive(Debug, Clone, Default)]
pub struct DeviceSummary {
    /// The UI must acknowledge this connection before sending bound writes.
    pub connection_id: u64,
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
    tx: Sender<Envelope>,
    rx: Receiver<Event>,
    handle: std::cell::RefCell<Option<thread::JoinHandle<()>>>,
    stop: Arc<AtomicBool>,
    disconnected_reported: std::cell::Cell<bool>,
    acknowledged_connection: std::cell::Cell<Option<u64>>,
}

impl Worker {
    /// 起一个工作线程。
    pub fn spawn(creds: Credentials, settings: Settings) -> Self {
        let (cmd_tx, cmd_rx) = channel::<Envelope>();
        let (evt_tx, evt_rx) = channel::<Event>();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let startup_events = evt_tx.clone();

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
                    w.run(cmd_rx, &thread_stop);
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
            ;
        let handle = match handle {
            Ok(handle) => Some(handle),
            Err(error) => {
                let _ = startup_events.send(Event::Log(format!("无法启动工作线程：{error}")));
                None
            }
        };

        Self { tx: cmd_tx, rx: evt_rx, handle: std::cell::RefCell::new(handle), stop,
            disconnected_reported: std::cell::Cell::new(false), acknowledged_connection: std::cell::Cell::new(None) }
    }

    /// 发指令（忽略发送失败——线程已退出时说明程序正在关闭）。
    pub fn send(&self, cmd: Command) {
        if self.stop.load(Ordering::SeqCst) { return; }
        let connection_id = is_device_write(&cmd).then(|| self.acknowledged_connection.get()).flatten();
        let _ = self.tx.send(Envelope { command: cmd, connection_id });
    }

    pub fn acknowledge_connection(&self, connection_id: u64) {
        self.acknowledged_connection.set(Some(connection_id));
    }

    /// 非阻塞取一个事件。界面定时器里调用，绝不阻塞重绘。
    pub fn try_recv(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(e) => Some(e),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) if !self.disconnected_reported.replace(true)
                && !self.stop.load(Ordering::SeqCst) => Some(Event::Stopped {
                    reason: "后台控制通道已停止，请重启程序".into()
                }),
            Err(TryRecvError::Disconnected) => None,
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
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Envelope { command: Command::Shutdown, connection_id: None });
        if let Some(h) = self.handle.borrow_mut().take() {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !h.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if h.is_finished() { let _ = h.join(); }
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
    cloud_login_required: bool,
    connection_id: u64,
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
            cloud_login_required: false,
            connection_id: 0,
        }
    }

    fn log(&self, msg: impl Into<String>) {
        let msg = msg.into();
        // 同时打到 stderr：界面日志区在窗口里，排查启动期问题时看不到，
        // 而无头运行（--probe / --self-test）时更没有界面。
        eprintln!("[工作线程] {msg}");
        let _ = self.tx.send(Event::Log(msg));
    }

    fn run(&mut self, rx: Receiver<Envelope>, stop: &AtomicBool) {
        self.log("已启动");
        loop {
            // Shutdown bypasses queued device commands. Only an already running
            // blocking operation can finish; queued writes never run on exit.
            if stop.load(Ordering::SeqCst) { break; }
            // 带超时的 recv：没有指令时定期做一次「自愈」（检查是否需要重连）
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Envelope { command: Command::Shutdown, .. }) => {
                    self.log("收到退出指令");
                    if let Some(c) = self.ctrl.as_mut() {
                        c.dispose();
                    }
                    break;
                }
                Ok(Envelope { command: cmd, connection_id }) => {
                    if stop.load(Ordering::SeqCst) { break; }
                    if is_device_write(&cmd) && connection_id != Some(self.connection_id) {
                        let error = ControllerError("写入所属连接已变化，旧操作已取消，请等待连接就绪后重试".into());
                        match cmd {
                            Command::WriteProp { name, .. } => self.write_completed(name, Err(error)),
                            _ => self.failed("控制写入", error),
                        }
                        continue;
                    }
                    self.handle(cmd)
                },
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
    ///
    /// 每条设备指令执行前都核对底层连接的绑定身份。登录切换设备时，
    /// 界面状态和 `Command::Init` 可能跨越同一个事件循环批次；如果仍复用
    /// 旧 Controller，就会出现“新名称、旧 DID”的竞态。
    fn ensure(&mut self) -> Result<ActiveLink, ControllerError> {
        let previously_connected = self.ctrl.is_some();
        let current = match self.creds.read_device() {
            Some(device) => device,
            None => {
                if let Some(c) = self.ctrl.as_mut() {
                    c.dispose();
                }
                self.ctrl = None;
                return Err(ControllerError(
                    "设备凭据已切换或尚未保存，请重新选择空调后再操作。".into(),
                ));
            }
        };
        if let Some(c) = self.ctrl.as_ref() {
            if let Some(link) = c.link {
                if c.is_bound_to(&current) && c.credentials_current() {
                    return Ok(link);
                }
            }
        }
        if let Some(c) = self.ctrl.as_mut() {
            c.dispose();
        }
        self.ctrl = None;
        let mut c = Controller::new(self.creds.clone(), self.settings.transport());
        let link = c.init_transport()?;
        self.ctrl = Some(c);
        self.connection_id = self.connection_id.wrapping_add(1).max(1);
        if previously_connected {
            let _ = self.tx.send(Event::Ready { link, device: self.device_summary() });
        }
        Ok(link)
    }

    fn ensure_write(&mut self) -> Result<ActiveLink, ControllerError> {
        // A queued click belongs to the connection the UI showed. Never move a
        // stale write to a newly logged-in account/device as part of ensure().
        let controller = self.ctrl.as_ref().ok_or_else(|| ControllerError("设备尚未连接，写入已取消".into()))?;
        if !controller.credentials_current() {
            return Err(ControllerError("设备或账号凭据已变化，写入已取消，请连接后重试".into()));
        }
        controller.link.ok_or_else(|| ControllerError("设备尚未连接，写入已取消".into()))
    }

    /// 设备信息摘要。
    fn device_summary(&self) -> DeviceSummary {
        let dir = self.creds.primary_dir().display().to_string();
        let has_cloud_session = self.creds.read_session().is_some_and(|s| s.ready());
        let has_thermometer = self
            .creds
            .read_thermometer()
            .is_some_and(|t| !t.did.trim().is_empty());

        // Ready must describe the connection that was actually initialized.
        // Login can replace device.json while profile loading is still running.
        let device = match self.ctrl.as_ref() {
            Some(controller) => controller.device().cloned(),
            None => self.creds.read_device(),
        };
        match device {
            Some(d) => DeviceSummary {
                connection_id: self.connection_id,
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
            Err(e) if e.requires_cloud_login() => {
                self.failures = 0;
                self.require_cloud_login();
            }
            Err(_) => self.failures = self.failures.saturating_add(1),
        }
    }

    fn require_cloud_login(&mut self) {
        if !self.cloud_login_required {
            self.cloud_login_required = true;
            let _ = self.tx.send(Event::CloudLoginRequired);
        }
    }

    fn failed(&mut self, op: impl Into<String>, error: ControllerError) {
        if error.requires_cloud_login() {
            self.require_cloud_login();
        }
        let _ = self.tx.send(Event::Failed { op: op.into(), error: error.to_string() });
    }

    fn write_completed(&mut self, name: String, result: Result<(), ControllerError>) {
        if result.as_ref().is_err_and(ControllerError::requires_cloud_login) {
            self.require_cloud_login();
        }
        let _ = self.tx.send(Event::Wrote {
            name,
            ok: result.is_ok(),
            error: result.err().map(|error| error.to_string()),
        });
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Shutdown => unreachable!("run() 里已单独处理"),

            Command::Init | Command::SetTransport(_) => {
                // Init 在启动/保存新登录凭据后发送；只切换通道不恢复过期云请求。
                if matches!(cmd, Command::Init) { self.cloud_login_required = false; }
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
                        self.failed("读状态", e);
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
                        self.failed("读诊断", e);
                    }
                }
            }

            Command::Thermometer => {
                if self.cloud_login_required { return; }
                // 温湿度计失败不影响空调控制，所以不参与重连计数
                if self.ensure().is_ok() {
                    let t = self.ctrl.as_mut().expect("有控制器").read_thermometer();
                    if t.reason.as_deref() == Some(crate::cloud::SESSION_EXPIRED_MESSAGE) {
                        self.require_cloud_login();
                    }
                    let _ = self.tx.send(Event::Thermometer(t));
                }
            }

            Command::PowerStats => {
                if self.cloud_login_required { return; }
                let r = self
                    .ensure()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").power_stats());
                match r {
                    Ok(p) => {
                        self.failures = 0;
                        let _ = self.tx.send(Event::PowerStats(Box::new(p)));
                    }
                    Err(e) => {
                        if e.requires_cloud_login() {
                            self.require_cloud_login();
                            return;
                        }
                        self.failed("读电量统计", e);
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
                        self.failed("读维护状态", e);
                    }
                }
            }

            Command::WriteProp { name, value } => {
                let r = self
                    .ensure_write()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").write_prop(&name, value));
                // A rejected value or a device business error is not evidence
                // that the transport is down. Reads drive reconnection counts.
                self.write_completed(name, r);
            }

            Command::RawWrite { siid, piid, value } => {
                let r = self
                    .ensure_write()
                    .and_then(|_| self.ctrl.as_mut().expect("有控制器").raw_write(siid, piid, value));
                match r {
                    Ok(v) => {
                        let _ = self.tx.send(Event::RawValue { siid, piid, value: v });
                    }
                    Err(e) => {
                        self.failed(format!("写 {siid}.{piid}"), e);
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
                        self.failed(format!("读 {siid}.{piid}"), e);
                    }
                }
            }

            Command::ApplyPreset(t) => {
                // Validate the input before touching/rebuilding the channel.
                // Invalid UI input is not evidence of a stale connection.
                if !t.is_finite() {
                    self.failed("应用温度预设", ControllerError("温度必须是有限数值".into()));
                    return;
                }
                let r = self
                    .ensure_write()
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
                        self.write_completed("targetTemp".into(), Ok(()));
                    }
                    Err(e) => {
                        // This can be validation or device-side rejection after
                        // a successful power read; it does not prove a disconnect.
                        self.failed("应用温度预设", e);
                    }
                }
            }

            Command::EncryptCredentials => {
                self.log("[凭据] 开始改用 DPAPI 加密存储…");
                let report = self.creds.upgrade_existing();
                let status = self.creds.protection_status();
                let mut failed = report.failures;
                if status.present == 0 && failed.is_empty() {
                    failed.push("尚未找到可加密的凭据".into());
                }
                if status.present > 0 && !status.active_files_protected() && failed.is_empty() {
                    failed.push("加密后校验未通过，请检查凭据文件".into());
                }
                if failed.is_empty() {
                    self.log(format!("[凭据] 已使用当前 Windows 用户的 DPAPI 加密 {} 个文件", status.current_format));
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
    fn a_disconnected_event_channel_reports_stopped_exactly_once() {
        let (tx, _commands) = channel();
        let (events, rx) = channel(); drop(events);
        let worker = Worker { tx, rx, handle: std::cell::RefCell::new(None),
            stop: Arc::new(AtomicBool::new(false)), disconnected_reported: std::cell::Cell::new(false), acknowledged_connection: std::cell::Cell::new(None) };
        assert!(matches!(worker.try_recv(), Some(Event::Stopped { .. })));
        assert!(worker.try_recv().is_none());
    }

    #[test]
    fn shutdown_cancels_queued_work_before_any_command_is_handled() {
        let (commands, rx) = channel();
        commands.send(Envelope { command:Command::Init, connection_id:None }).unwrap();
        commands.send(Envelope { command:Command::WriteProp { name:"on".into(), value:Value::Bool(true) }, connection_id:Some(1) }).unwrap();
        let (events, result) = channel();
        let mut worker = WorkerLoop::new(Credentials::isolated(std::env::temp_dir().join("unused-stopped-worker")), Settings::default(), events);
        worker.run(rx, &AtomicBool::new(true));
        assert!(result.try_iter().all(|event| matches!(event, Event::Log(_))));
        assert!(worker.ctrl.is_none());
    }

    #[test]
    fn dequeuing_ready_does_not_rebind_writes_before_the_ui_accepts_it() {
        let (tx, commands) = channel();
        let (events, rx) = channel();
        let worker = Worker { tx, rx, handle: std::cell::RefCell::new(None),
            stop: Arc::new(AtomicBool::new(false)), disconnected_reported: std::cell::Cell::new(false),
            acknowledged_connection: std::cell::Cell::new(Some(1)) };
        events.send(Event::Ready { link:ActiveLink::Cloud,
            device:DeviceSummary { connection_id:2, ..Default::default() } }).unwrap();
        assert!(matches!(worker.try_recv(), Some(Event::Ready { .. })));
        worker.send(Command::WriteProp { name:"on".into(), value:Value::Bool(true) });
        assert_eq!(commands.recv().unwrap().connection_id, Some(1));
        worker.acknowledge_connection(2);
        worker.send(Command::WriteProp { name:"on".into(), value:Value::Bool(true) });
        assert_eq!(commands.recv().unwrap().connection_id, Some(2));
    }

    #[test]
    fn a_queued_write_is_cancelled_after_the_worker_rebuilds_its_connection() {
        let (commands, rx) = channel();
        commands.send(Envelope { command:Command::WriteProp { name:"on".into(), value:Value::Bool(true) }, connection_id:Some(1) }).unwrap();
        drop(commands);
        let (events, results) = channel();
        let mut worker = WorkerLoop::new(Credentials::isolated(std::env::temp_dir().join("unused-bound-command")), Settings::default(), events);
        worker.connection_id = 2;
        worker.run(rx, &AtomicBool::new(false));
        assert!(results.try_iter().any(|event| matches!(event,
            Event::Wrote { ok:false, error:Some(error), .. } if error.contains("旧操作已取消"))));
    }

    #[test]
    fn ensure_rebuilds_an_old_channel_after_device_credentials_switch_without_init() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("miac-device-switch-ensure-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let device_path = directory.join(crate::credentials::FILE_DEVICE);
        let old_device = crate::credentials::DeviceInfo {
            name: Some("old fixture".into()), model: Some(crate::miot::EXPECT_MODEL.into()), did: "1".into(),
            localip: Some("127.0.0.1".into()), token: Some("11111111111111111111111111111111".into()),
            ..Default::default()
        };
        std::fs::write(&device_path, serde_json::to_vec(&old_device).unwrap()).unwrap();

        let creds = Credentials::isolated(&directory);
        let mut worker = WorkerLoop::new(creds.clone(), Settings::default(), channel().0);
        let mut old_controller = Controller::new(creds, Transport::Local);
        assert_eq!(old_controller.init_transport().unwrap(), ActiveLink::Local);
        assert!(old_controller.is_bound_to(&old_device));
        worker.ctrl = Some(old_controller);

        let new_device = crate::credentials::DeviceInfo {
            name: Some("new fixture".into()), model: Some(crate::miot::EXPECT_MODEL.into()), did: "2".into(),
            localip: Some("127.0.0.2".into()), token: Some("22222222222222222222222222222222".into()),
            ..Default::default()
        };
        std::fs::write(&device_path, serde_json::to_vec(&new_device).unwrap()).unwrap();

        assert_eq!(worker.ensure().unwrap(), ActiveLink::Local);
        let controller = worker.ctrl.as_ref().unwrap();
        assert_eq!(controller.device().unwrap().did, "2");
        assert!(controller.is_bound_to(&new_device));
        assert!(!controller.is_bound_to(&old_device));

        std::fs::remove_file(&device_path).unwrap();
        assert!(worker.ensure().is_err(), "missing switched credentials must not reuse the old channel");
        assert!(worker.ctrl.is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn ready_summary_uses_the_connected_device_when_disk_credentials_change() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("miac-ready-summary-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let device_path = directory.join(crate::credentials::FILE_DEVICE);
        let old_device = crate::credentials::DeviceInfo {
            name: Some("old fixture".into()), model: Some(crate::miot::EXPECT_MODEL.into()), did: "1".into(),
            localip: Some("127.0.0.1".into()), token: Some("00000000000000000000000000000000".into()),
            ..Default::default()
        };
        std::fs::write(&device_path, serde_json::to_vec(&old_device).unwrap()).unwrap();
        let creds = Credentials::isolated(&directory);
        let mut controller = Controller::new(creds.clone(), Transport::Local);
        // init_transport constructs the loopback socket but sends no packets.
        assert_eq!(controller.init_transport().unwrap(), ActiveLink::Local);
        let (tx, _rx) = channel();
        let mut worker = WorkerLoop::new(creds, Settings::default(), tx);
        worker.ctrl = Some(controller);

        let new_device = crate::credentials::DeviceInfo {
            name: Some("new fixture".into()), model: Some("xiaomi.aircondition.ma7".into()), did: "2".into(),
            localip: Some("127.0.0.2".into()), ..old_device
        };
        std::fs::write(&device_path, serde_json::to_vec(&new_device).unwrap()).unwrap();
        let summary = worker.device_summary();
        assert_eq!(summary.did, "1", "Ready must identify the device of the actual controller");
        assert_eq!(summary.name, "old fixture");
        assert_eq!(summary.model, crate::miot::EXPECT_MODEL);
        assert_eq!(summary.profile.model, summary.model);
        assert_eq!(summary.localip.as_deref(), Some("127.0.0.1"));

        std::fs::remove_file(&device_path).unwrap();
        assert_eq!(worker.device_summary().did, "1", "a missing disk file must not erase the connected device");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_temperature_preset_does_not_trigger_reconnection() {
        let creds = Credentials::isolated(std::env::temp_dir().join("miac-invalid-preset-worker-test"));
        let (tx, rx) = channel();
        let mut worker = WorkerLoop::new(creds.clone(), Settings::default(), tx);
        let mut controller = Controller::new(creds, Transport::Local);
        controller.profile = crate::profile::Profile::legacy();
        controller.link = Some(ActiveLink::Local);
        worker.ctrl = Some(controller);

        for _ in 0..3 {
            worker.handle(Command::ApplyPreset(f64::NAN));
            assert!(matches!(rx.try_recv(), Ok(Event::Failed { .. })));
        }
        assert_eq!(worker.failures, 0, "invalid write values must not be counted as transport failures");
        assert_eq!(worker.ctrl.as_ref().unwrap().link, Some(ActiveLink::Local));
    }

    #[test]
    fn expired_failures_notify_login_before_the_consumer_discards_the_error() {
        let creds = Credentials::isolated(std::env::temp_dir().join("miac-expired-result-worker-test"));
        let (tx, rx) = channel();
        let mut worker = WorkerLoop::new(creds, Settings::default(), tx);
        worker.failures = 2;

        worker.failed("读维护状态", crate::cloud::CloudError::SessionExpired.into());
        assert!(matches!(rx.try_recv(), Ok(Event::CloudLoginRequired)));
        assert!(matches!(rx.try_recv(), Ok(Event::Failed { op, error })
            if op == "读维护状态" && error == crate::cloud::SESSION_EXPIRED_MESSAGE));
        worker.failed("读 2.1", crate::cloud::CloudError::SessionExpired.into());
        assert!(matches!(rx.try_recv(), Ok(Event::Failed { op, .. }) if op == "读 2.1"));
        assert!(rx.try_recv().is_err(), "one login prompt per login cycle");
        assert_eq!(worker.failures, 2);
    }

    #[test]
    fn expired_write_notifies_login_and_keeps_the_failed_write_result() {
        let creds = Credentials::isolated(std::env::temp_dir().join("miac-expired-write-worker-test"));
        let (tx, rx) = channel();
        let mut worker = WorkerLoop::new(creds, Settings::default(), tx);

        worker.write_completed("mode".into(), Err(crate::cloud::CloudError::SessionExpired.into()));
        assert!(matches!(rx.try_recv(), Ok(Event::CloudLoginRequired)));
        assert!(matches!(rx.try_recv(), Ok(Event::Wrote { name, ok: false, error: Some(error) })
            if name == "mode" && error == crate::cloud::SESSION_EXPIRED_MESSAGE));
        worker.write_completed("mode".into(), Err(ControllerError("device rejected value".into())));
        assert!(matches!(rx.try_recv(), Ok(Event::Wrote { ok: false, .. })));
        assert!(rx.try_recv().is_err());
        assert_eq!(worker.failures, 0);
    }

    #[test]
    fn expired_session_notifies_once_and_suppresses_only_cloud_supplementary_reads() {
        let creds = Credentials::isolated(std::env::temp_dir().join("miac-expired-worker-test"));
        let (tx, rx) = channel();
        let mut worker = WorkerLoop::new(creds.clone(), Settings::default(), tx);
        let mut controller = Controller::new(creds, Transport::Auto);
        controller.link = Some(ActiveLink::Local);
        worker.ctrl = Some(controller);
        worker.note(&Err(crate::cloud::CloudError::SessionExpired.into()));
        assert!(matches!(rx.try_recv(), Ok(Event::CloudLoginRequired)));
        worker.require_cloud_login();
        worker.handle(Command::PowerStats);
        worker.handle(Command::Thermometer);
        assert!(rx.try_recv().is_err());
        assert_eq!(worker.failures, 0);
        assert_eq!(worker.ctrl.as_ref().unwrap().link, Some(ActiveLink::Local));
        worker.handle(Command::Init);
        assert!(!worker.cloud_login_required);
    }

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
