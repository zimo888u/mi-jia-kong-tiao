// ─────────────────────────────────────────────────────────────────────────────
// login_ui.rs —— 扫码登录的界面粘合层
//
// 把 `miac_core::login`（纯逻辑、阻塞式 HTTP）接到 Slint 界面上：
//
//   界面点击「扫码登录」──LoginCmd──▶ 登录线程（LoginSession + 轮询）
//                        ◀─LoginEvent──
//
// 与主控制核心一样，网络调用不在 UI 线程上跑：申请二维码要一次 HTTPS，
// 轮询要持续到用户扫码确认（可能几十秒），放在 UI 线程会整窗卡死。
//
// 登录成功后：写 cloud-session.json → 拉设备列表 → 用户选设备 →
// 写 device.json / thermometer.json。之后由调用方重新初始化控制通道。
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use miac_core::credentials::Credentials;
#[cfg(test)]
use miac_core::credentials::FILE_THERMOMETER;
use miac_core::login::{self, CloudDevice};
use miac_core::miot;

/// 界面发给登录线程的指令。
#[derive(Debug, Clone, Copy)]
pub enum LoginCmd {
    /// 开始（或重新开始）扫码流程
    Start,
    /// 用户选中了第 n 个设备并确认
    Confirm(usize),
    /// 取消
    Cancel,
    /// 程序退出，协调线程必须结束，供 JoinHandle 收尾。
    Shutdown,
}

/// 登录线程回给界面的事件。
pub enum LoginEvent {
    /// 二维码已就绪（原始 PNG 字节 + 剩余秒数）
    Qr { png: Vec<u8>, seconds: u64 },
    /// 申请二维码失败
    Failed(String),
    /// 已确认，拿到了设备列表
    Devices(Vec<CloudDevice>),
    /// 凭据已写入
    Saved { device: String, thermometer: Option<String> },
    /// 进度提示（显示在弹层下方）
    Status(String),
}

/// Commands and results stay bound to the scan that produced them. Advancing the
/// generation on the UI thread invalidates even results already in the queue.
struct Tagged<T> {
    id: u64,
    payload: T,
}

fn current_payload<T>(message: Tagged<T>, generation: &AtomicU64) -> Option<T> {
    (message.id == generation.load(Ordering::SeqCst)).then_some(message.payload)
}

fn emit_current(tx: &Sender<Tagged<LoginEvent>>, id: u64, generation: &AtomicU64, event: LoginEvent) {
    if id == generation.load(Ordering::SeqCst) {
        let _ = tx.send(Tagged { id, payload: event });
    }
}

fn active(id: u64, generation: &AtomicU64, cancel: &AtomicBool) -> bool {
    id == generation.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst)
}

fn emit_active(
    tx: &Sender<Tagged<LoginEvent>>, id: u64, generation: &AtomicU64,
    cancel: &AtomicBool, event: LoginEvent,
) {
    if active(id, generation, cancel) {
        emit_current(tx, id, generation, event);
    }
}

fn panic_message(error: &(dyn std::any::Any + Send)) -> String {
    error.downcast_ref::<&str>().map(|s| (*s).to_string())
        .or_else(|| error.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "未知 panic".into())
}

type PendingLogin = (u64, Vec<CloudDevice>, miac_core::cloud::CloudSession);

fn complete_login(
    tx: &Sender<Tagged<LoginEvent>>, cancel: &AtomicBool,
    completed: &Mutex<Option<PendingLogin>>, id: u64, generation: &AtomicU64,
    devices: Vec<CloudDevice>, session: miac_core::cloud::CloudSession,
) {
    if let Ok(mut slot) = completed.lock() {
        // Check while holding the slot: a stale thread waiting for this lock
        // must never replace a session already completed by the newer scan.
        if !active(id, generation, cancel) { return; }
        *slot = Some((id, devices.clone(), session));
    } else {
        emit_active(tx, id, generation, cancel, LoginEvent::Failed("无法接收登录会话，请重新扫码".into()));
        return;
    }
    emit_active(tx, id, generation, cancel, LoginEvent::Devices(devices));
}

/// 登录线程句柄。
pub struct LoginWorker {
    tx: Sender<Tagged<LoginCmd>>,
    rx: Receiver<Tagged<LoginEvent>>,
    event_tx: Sender<Tagged<LoginEvent>>,
    generation: Arc<AtomicU64>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LoginWorker {
    /// 起线程。调用方用 `drain` 在定时器里非阻塞取事件。
    pub fn spawn(creds: Credentials) -> Self {
        let (cmd_tx, cmd_rx) = channel::<Tagged<LoginCmd>>();
        let (evt_tx, evt_rx) = channel::<Tagged<LoginEvent>>();
        let generation = Arc::new(AtomicU64::new(0));
        let thread_generation = generation.clone();
        let thread_events = evt_tx.clone();

        let handle = thread::Builder::new()
            .name("miac-login".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(creds, cmd_rx, thread_events.clone(), thread_generation.clone());
                }));
                if let Err(e) = r {
                    let msg = panic_message(e.as_ref());
                    eprintln!("[登录线程] 已崩溃：{msg}");
                    let id = thread_generation.load(Ordering::SeqCst);
                    emit_current(&thread_events, id, &thread_generation, LoginEvent::Failed(format!("登录线程异常：{msg}")));
                }
            });
        let handle = match handle {
            Ok(handle) => Some(handle),
            Err(error) => {
                emit_current(&evt_tx, 0, &generation, LoginEvent::Failed(format!("无法启动登录线程：{error}")));
                None
            }
        };

        Self { tx: cmd_tx, rx: evt_rx, event_tx: evt_tx, generation, handle }
    }

    pub fn send(&self, cmd: LoginCmd) {
        let id = match cmd {
            LoginCmd::Start | LoginCmd::Cancel | LoginCmd::Shutdown =>
                self.generation.fetch_add(1, Ordering::SeqCst) + 1,
            LoginCmd::Confirm(_) => self.generation.load(Ordering::SeqCst),
        };
        if self.tx.send(Tagged { id, payload: cmd }).is_err() && !matches!(cmd, LoginCmd::Shutdown) {
            emit_current(&self.event_tx, id, &self.generation, LoginEvent::Failed("登录线程已停止，无法处理登录请求".into()));
        }
    }

    /// 非阻塞取事件（界面定时器里调用）。
    pub fn drain(&self, out: &mut Vec<LoginEvent>) {
        loop {
            match self.rx.try_recv() {
                Ok(message) => {
                    if let Some(event) = current_payload(message, &self.generation) { out.push(event); }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
            if out.len() >= 32 {
                break;
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.send(LoginCmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !h.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if h.is_finished() { let _ = h.join(); }
        }
    }
}

impl Drop for LoginWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 把二维码 PNG 解成 RGBA，交给 Slint 显示。
///
/// 这一步不能省：小米返回的是 PNG 压缩数据，Slint 的 `Image` 需要解码后的像素。
/// 用 `png` crate 只解析 PNG，不引入完整图像生态。
pub fn png_to_rgba(png_bytes: &[u8]) -> Option<slint::Image> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width == 0 || height == 0 || width > 2048 || height > 2048 { return None; }
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;

    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 || w > 2048 || h > 2048 {
        return None;
    }

    // PNG 可能是灰阶/调色板/RGB/RGBA，统一转成 RGBA8
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => {
            buf[..info.buffer_size()].iter().flat_map(|g| [*g, *g, *g, 255]).collect()
        }
        png::ColorType::GrayscaleAlpha => buf[..info.buffer_size()]
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => {
            // 调色板图：用 plte + trns 展开
            let plte = reader.info().palette.clone().unwrap_or_default();
            let trns = reader.info().trns.clone();
            buf[..info.buffer_size()]
                .iter()
                .flat_map(|idx| {
                    let i = (*idx as usize) * 3;
                    let r = plte.get(i).copied().unwrap_or(255);
                    let g = plte.get(i + 1).copied().unwrap_or(255);
                    let b = plte.get(i + 2).copied().unwrap_or(255);
                    let a = trns.as_ref().and_then(|t| t.get(*idx as usize).copied()).unwrap_or(255);
                    [r, g, b, a]
                })
                .collect()
        }
    };

    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }

    let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&rgba, w, h);
    Some(slint::Image::from_rgba8(pixel_buf))
}

/// 登录线程主循环。
fn run(
    creds: Credentials, rx: Receiver<Tagged<LoginCmd>>, tx: Sender<Tagged<LoginEvent>>,
    generation: Arc<AtomicU64>,
) {
    // 已经走完流程、正等用户选设备时，把设备列表留在这里
    let mut pending_devices: Option<PendingLogin> = None;
    let mut cancel_flag = Arc::new(AtomicBool::new(false));
    // 登录会话绝不发给 UI；仅在协调线程和登录流线程之间私下传递。
    let completed = Arc::new(Mutex::new(None::<PendingLogin>));

    loop {
        let message = match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(c) => c,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some((id, devices, session)) = completed.lock().ok().and_then(|mut s| s.take()) {
                    if id == generation.load(Ordering::SeqCst) {
                        pending_devices = Some((id, devices, session));
                    }
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                cancel_flag.store(true, Ordering::SeqCst);
                break;
            }
        };
        let id = message.id;
        let Some(cmd) = current_payload(message, &generation) else { continue; };

        // Devices 事件在 UI 侧可见前，登录流已经把敏感会话放入 completed；
        // 必须先接手它，再处理 Confirm，避免“看得到设备却不能保存”的竞态。
        if let Some((id, devices, session)) = completed.lock().ok().and_then(|mut s| s.take()) {
            if id == generation.load(Ordering::SeqCst) {
                pending_devices = Some((id, devices, session));
            }
        }

        match cmd {
            LoginCmd::Shutdown => {
                cancel_flag.store(true, Ordering::SeqCst);
                break;
            }
            LoginCmd::Cancel => {
                cancel_flag.store(true, Ordering::SeqCst);
                pending_devices = None;
                if let Ok(mut slot) = completed.lock() {
                    *slot = None;
                }
                // 让线程回到等待下一条指令的状态（不退出，界面可能再次登录）
                emit_current(&tx, id, &generation, LoginEvent::Status("已取消".into()));
            }

            LoginCmd::Start => {
                // 使旧流程失效，防止它在稍后覆盖新二维码的设备会话。
                cancel_flag.store(true, Ordering::SeqCst);
                cancel_flag = Arc::new(AtomicBool::new(false));
                pending_devices = None;
                if let Ok(mut slot) = completed.lock() {
                    *slot = None;
                }
                let cancel = cancel_flag.clone();
                let tx2 = tx.clone();
                let completed2 = completed.clone();
                let generation2 = generation.clone();

                // 登录本身再开一层：这样界面在等待期间仍能响应「取消」
                let flow = thread::Builder::new()
                    .name("miac-login-flow".into())
                    .stack_size(512 * 1024)
                    .spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            do_login(&tx2, &cancel, &completed2, id, &generation2);
                        }));
                        if let Err(error) = result {
                            emit_active(&tx2, id, &generation2, &cancel,
                                LoginEvent::Failed(format!("扫码登录线程异常：{}", panic_message(error.as_ref()))));
                        }
                    });
                if let Err(error) = flow {
                    emit_current(&tx, id, &generation, LoginEvent::Failed(format!("无法启动扫码登录线程：{error}")));
                }
            }

            LoginCmd::Confirm(idx) => {
                let Some((pending_id, devices, session)) = pending_devices.take() else {
                    emit_current(&tx, id, &generation, LoginEvent::Failed("还没有可保存的登录会话".into()));
                    continue;
                };
                if pending_id != id || id != generation.load(Ordering::SeqCst) {
                    emit_current(&tx, id, &generation, LoginEvent::Failed("登录会话已失效，请重新扫码".into()));
                    continue;
                }
                match login::save_credentials_if(&creds, &session, &devices, idx,
                    || id == generation.load(Ordering::SeqCst)) {
                    Ok((device_name, thermo)) => {
                        emit_current(&tx, id, &generation, LoginEvent::Saved { device: device_name, thermometer: thermo });
                    }
                    Err(e) => {
                        // 保存失败也把会话留下，用户还能重试选设备
                        if id == generation.load(Ordering::SeqCst) {
                            pending_devices = Some((id, devices, session));
                        }
                        emit_current(&tx, id, &generation, LoginEvent::Failed(e));
                    }
                }
            }
        }

    }
}

/// 真正的登录流程（跑在子线程里）。
fn do_login(
    tx: &Sender<Tagged<LoginEvent>>,
    cancel: &AtomicBool,
    completed: &Mutex<Option<PendingLogin>>,
    id: u64,
    generation: &AtomicU64,
) {
    if !active(id, generation, cancel) { return; }
    emit_active(tx, id, generation, cancel, LoginEvent::Status("正在向小米申请二维码…".into()));
    let mut session = login::LoginSession::new(None);

    let challenge = match session.create_challenge() {
        Ok(c) => c,
        Err(e) => {
            emit_active(tx, id, generation, cancel, LoginEvent::Failed(e.to_string()));
            return;
        }
    };
    let seconds = challenge.expires_at.saturating_duration_since(Instant::now()).as_secs();
    if !active(id, generation, cancel) { return; }
    emit_active(tx, id, generation, cancel, LoginEvent::Qr { png: challenge.image.clone(), seconds });

    let result = match session.wait_for_approval(&challenge, || active(id, generation, cancel)) {
        Ok(v) => v,
        Err(login::LoginError::Cancelled) => {
            emit_active(tx, id, generation, cancel, LoginEvent::Status("已取消登录".into()));
            return;
        }
        Err(e) => {
            emit_active(tx, id, generation, cancel, LoginEvent::Failed(e.to_string()));
            return;
        }
    };

    if !active(id, generation, cancel) { return; }
    let session_data = result.to_session(miot::COUNTRY);

    emit_active(tx, id, generation, cancel, LoginEvent::Status("扫码成功，正在读取空调和温湿度计…".into()));
    let devices = match login::fetch_devices(&session_data, None) {
        Ok(d) => d,
        Err(e) => {
            emit_active(tx, id, generation, cancel, LoginEvent::Failed(e.to_string()));
            return;
        }
    };
    if devices.is_empty() {
        emit_active(tx, id, generation, cancel, LoginEvent::Failed("账号下没有找到空调或温湿度计".into()));
        return;
    }
    if !devices.iter().any(CloudDevice::looks_like_ac) {
        emit_active(tx, id, generation, cancel, LoginEvent::Failed("账号下只找到温湿度计，没有可控制的空调".into()));
        return;
    }

    // 会话要留着（保存凭据时一起写），通过私有槽交给协调线程；已取消/过期
    // 的旧流程没有资格覆盖当前流程。
    complete_login(tx, cancel, completed, id, generation, devices, session_data);
}

/// 写三个凭据文件。返回 (空调名, 温湿度计名)。
#[cfg(test)]
fn save_credentials(
    creds: &Credentials,
    session: &miac_core::cloud::CloudSession,
    devices: &[CloudDevice],
    idx: usize,
) -> Result<(String, Option<String>), String> {
    login::save_credentials(creds, session, devices, idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_credentials(case: &str) -> Credentials {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = workspace.join("target").join("fresh-audit-tests")
            .join(format!("login-ui-{case}-{}-{unique}", std::process::id()));
        Credentials::isolated(directory)
    }

    fn fake_ac() -> CloudDevice {
        CloudDevice::from_json(&serde_json::json!({
            "did": "fake-new-account-ac", "model": miot::EXPECT_MODEL, "name": "测试空调"
        })).unwrap()
    }

    fn fake_sensor() -> CloudDevice {
        CloudDevice::from_json(&serde_json::json!({
            "did": "fake-old-account-sensor", "model": "miaomiaoce.sensor_ht.t9", "name": "测试温湿度计"
        })).unwrap()
    }

    fn fake_session() -> miac_core::cloud::CloudSession {
        miac_core::cloud::CloudSession {
            username: "fake-new-account".into(), user_id: "fake-new-account".into(),
            ssecurity: "MDEyMzQ1Njc4OWFiY2RlZg==".into(), service_token: "fake-token".into(),
            ..Default::default()
        }
    }

    fn fake_worker() -> (LoginWorker, Receiver<Tagged<LoginCmd>>, Sender<Tagged<LoginEvent>>) {
        let (cmd_tx, cmd_rx) = channel();
        let (evt_tx, evt_rx) = channel();
        let worker = LoginWorker {
            tx: cmd_tx, rx: evt_rx, event_tx: evt_tx.clone(),
            generation: Arc::new(AtomicU64::new(0)), handle: None,
        };
        (worker, cmd_rx, evt_tx)
    }

    #[test]
    fn fresh_audit_save_ac_only_clears_previous_account_sensor() {
        let creds = fake_credentials("ac-only");
        creds.write_json(FILE_THERMOMETER, &serde_json::json!({
            "did": "fake-old-account-sensor", "model": "miaomiaoce.sensor_ht.t9"
        })).unwrap();
        let session = fake_session();
        let result = save_credentials(&creds, &session, &[fake_ac()], 0).unwrap();
        assert_eq!(result.1, None);
        assert!(creds.read_thermometer().is_none(), "AC-only login must clear the previous account's sensor");
        let marker = creds.read_json::<miac_core::credentials::ThermometerInfo>(FILE_THERMOMETER).unwrap();
        assert!(marker.did.is_empty(), "an encrypted no-sensor marker must block fallback to an old account");
        assert!(creds.protection_status().active_files_protected());
        assert_eq!(creds.read_session().unwrap().username, "fake-new-account");
    }

    #[test]
    fn fresh_audit_save_reports_atomic_commit_failure() {
        let creds = fake_credentials("sensor-write-failure");
        std::fs::create_dir_all(creds.primary_dir().join(miac_core::credentials::FILE_LOGIN)).unwrap();
        let result = save_credentials(&creds, &fake_session(), &[fake_ac(), fake_sensor()], 0);
        assert!(result.is_err(), "a failed sensor write must not report Saved success");
        assert!(result.unwrap_err().contains("保存登录状态失败"));
    }

    #[test]
    fn fresh_audit_cancel_discards_queued_qr_and_late_failure() {
        let (worker, _cmd_rx, evt_tx) = fake_worker();
        worker.send(LoginCmd::Start);
        let id = worker.generation.load(Ordering::SeqCst);
        evt_tx.send(Tagged { id, payload: LoginEvent::Qr { png: vec![], seconds: 300 } }).unwrap();
        worker.send(LoginCmd::Cancel);
        evt_tx.send(Tagged { id, payload: LoginEvent::Failed("old HTTP result".into()) }).unwrap();
        let mut out = vec![];
        worker.drain(&mut out);
        assert!(out.is_empty(), "cancel must discard results already queued and produced afterward");
        let current = worker.generation.load(Ordering::SeqCst);
        evt_tx.send(Tagged { id: current, payload: LoginEvent::Status("已取消".into()) }).unwrap();
        worker.drain(&mut out);
        assert!(matches!(&out[..], [LoginEvent::Status(text)] if text == "已取消"));
    }

    #[test]
    fn fresh_audit_new_scan_discards_previous_flow_results() {
        let (worker, _cmd_rx, evt_tx) = fake_worker();
        worker.send(LoginCmd::Start);
        let id = worker.generation.load(Ordering::SeqCst);
        evt_tx.send(Tagged { id, payload: LoginEvent::Status("old QR request".into()) }).unwrap();
        worker.send(LoginCmd::Start);
        let mut out = vec![];
        worker.drain(&mut out);
        assert!(out.is_empty(), "old flow must never overwrite a newly requested QR/status");
        let current = worker.generation.load(Ordering::SeqCst);
        evt_tx.send(Tagged { id: current, payload: LoginEvent::Qr { png: vec![1], seconds: 300 } }).unwrap();
        worker.drain(&mut out);
        assert!(matches!(&out[..], [LoginEvent::Qr { png, seconds: 300 }] if png == &[1]));
    }

    #[test]
    fn fresh_audit_restart_discards_old_confirm_commands() {
        let (worker, cmd_rx, _evt_tx) = fake_worker();
        worker.send(LoginCmd::Start);
        worker.send(LoginCmd::Confirm(0));
        worker.send(LoginCmd::Start);
        let commands: Vec<_> = cmd_rx.try_iter()
            .filter_map(|message| current_payload(message, &worker.generation)).collect();
        assert!(matches!(&commands[..], [LoginCmd::Start]), "old Confirm must not save a newer pending session");
        worker.send(LoginCmd::Confirm(1));
        assert!(matches!(current_payload(cmd_rx.try_recv().unwrap(), &worker.generation), Some(LoginCmd::Confirm(1))));
    }

    #[test]
    fn fresh_audit_shutdown_invalidates_queued_results() {
        let (mut worker, _cmd_rx, evt_tx) = fake_worker();
        worker.send(LoginCmd::Start);
        let id = worker.generation.load(Ordering::SeqCst);
        evt_tx.send(Tagged { id, payload: LoginEvent::Status("old request".into()) }).unwrap();
        worker.shutdown();
        let mut out = vec![];
        worker.drain(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn fresh_audit_old_completion_cannot_overwrite_new_session() {
        let (worker, _cmd_rx, evt_tx) = fake_worker();
        let completed = Mutex::new(None);
        let cancel = AtomicBool::new(false);
        worker.send(LoginCmd::Start);
        let old_id = worker.generation.load(Ordering::SeqCst);
        worker.send(LoginCmd::Start);
        let current = worker.generation.load(Ordering::SeqCst);
        let new_session = miac_core::cloud::CloudSession { username: "fake-new-account".into(), ..Default::default() };
        complete_login(&evt_tx, &cancel, &completed, current, &worker.generation, vec![fake_ac()], new_session);
        complete_login(&evt_tx, &cancel, &completed, old_id, &worker.generation, vec![fake_ac()], Default::default());
        let guard = completed.lock().unwrap();
        let pending = guard.as_ref().unwrap();
        assert_eq!(pending.0, current);
        assert_eq!(pending.2.username, "fake-new-account");
        let mut out = vec![];
        worker.drain(&mut out);
        assert!(matches!(&out[..], [LoginEvent::Devices(devices)] if devices.len() == 1));
        drop(guard);
        cancel.store(true, Ordering::SeqCst);
        emit_active(&evt_tx, current, &worker.generation, &cancel, LoginEvent::Failed("cancelled request".into()));
        out.clear();
        worker.drain(&mut out);
        assert!(out.is_empty(), "even same-generation flow events must honor cancellation");
    }

    #[test]
    fn fresh_audit_stopped_worker_reports_failure() {
        let (worker, cmd_rx, _evt_tx) = fake_worker();
        drop(cmd_rx);
        worker.send(LoginCmd::Start);
        let mut out = vec![];
        worker.drain(&mut out);
        assert!(matches!(&out[..], [LoginEvent::Failed(text)] if text.contains("登录线程已停止")));
    }

    fn encoded_png(color: png::ColorType, depth: png::BitDepth, width: u32, data: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        {
            let mut encoder = png::Encoder::new(&mut out, width, 1);
            encoder.set_color(color); encoder.set_depth(depth);
            if color == png::ColorType::Indexed {
                encoder.set_palette(vec![0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0, 255]);
                encoder.set_trns(vec![255, 255, 128, 0]);
            }
            encoder.write_header().unwrap().write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn fresh_audit_png_accepts_packed_monochrome_and_palette_images() {
        let png = encoded_png(png::ColorType::Grayscale, png::BitDepth::One, 8, &[0b1010_0101]);
        let image = png_to_rgba(&png).expect("a valid 1-bit QR image must decode").to_rgba8().unwrap();
        let grayscale: Vec<_> = image.as_slice().iter().map(|p| p.r).collect();
        assert_eq!(grayscale, vec![255, 0, 255, 0, 0, 255, 0, 255]);
        let png = encoded_png(png::ColorType::Indexed, png::BitDepth::Two, 4, &[0b00_01_10_11]);
        let image = png_to_rgba(&png).expect("packed palette images must decode").to_rgba8().unwrap();
        let pixels: Vec<_> = image.as_slice().iter().map(|p| (p.r, p.g, p.b, p.a)).collect();
        assert_eq!(pixels, vec![(0,0,0,255), (255,255,255,255), (255,0,0,128), (0,0,255,0)]);
    }

    #[test]
    fn fresh_audit_png_accepts_16_bit_images_without_channel_corruption() {
        let png = encoded_png(png::ColorType::Rgba, png::BitDepth::Sixteen, 1, &[0x12,0x34,0x56,0x78,0x9a,0xbc,0xff,0xff]);
        let image = png_to_rgba(&png).expect("16-bit image must be normalized to RGBA8").to_rgba8().unwrap();
        let p = image.as_slice()[0];
        assert_eq!((p.r, p.g, p.b, p.a), (0x12, 0x56, 0x9a, 255));
    }

    #[test]
    fn thermometer_cannot_be_saved_as_the_controlled_air_conditioner() {
        let directory = std::env::temp_dir().join(format!("miac-login-reject-{}", std::process::id()));
        let creds = Credentials::isolated(&directory);
        let devices = [CloudDevice::from_json(&serde_json::json!({
            "did":"test-thermo", "model":"miaomiaoce.sensor_ht.t9", "name":"卧室"
        })).unwrap()];
        let error = save_credentials(&creds, &Default::default(), &devices, 0).unwrap_err();
        assert_eq!(error, "请选择空调，温湿度计会自动关联");
        assert!(!directory.exists(), "拒绝温湿度计选择时不能写入或读取凭据");
    }

    #[test]
    fn png_to_rgba_rejects_garbage() {
        assert!(png_to_rgba(b"not a png").is_none());
        assert!(png_to_rgba(&[]).is_none());
    }

    #[test]
    fn png_to_rgba_decodes_a_real_png() {
        // 用 png crate 自己编一张 2x2 RGBA 图，再解回来
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            let data: [u8; 16] = [
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 255, 255, 255, 255,
            ];
            w.write_image_data(&data).unwrap();
        }
        let img = png_to_rgba(&out);
        assert!(img.is_some(), "合法的 PNG 应能解出图像");
        let img = img.unwrap();
        assert_eq!(img.size().width, 2);
        assert_eq!(img.size().height, 2);
    }

    #[test]
    fn png_to_rgba_decodes_rgb_and_gray() {
        for color in [png::ColorType::Rgb, png::ColorType::Grayscale] {
            let mut out = Vec::new();
            let channels = if color == png::ColorType::Rgb { 3 } else { 1 };
            {
                let mut enc = png::Encoder::new(&mut out, 3, 1);
                enc.set_color(color);
                enc.set_depth(png::BitDepth::Eight);
                let mut w = enc.write_header().unwrap();
                w.write_image_data(&vec![128u8; 3 * channels]).unwrap();
            }
            let img = png_to_rgba(&out).expect("应能解码");
            assert_eq!(img.size().width, 3);
        }
    }
}
