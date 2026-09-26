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
use std::thread;
use std::time::{Duration, Instant};

use miac_core::credentials::{Credentials, FILE_DEVICE, FILE_SESSION, FILE_THERMOMETER};
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

/// 登录线程句柄。
pub struct LoginWorker {
    tx: Sender<LoginCmd>,
    rx: Receiver<LoginEvent>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LoginWorker {
    /// 起线程。调用方用 `drain` 在定时器里非阻塞取事件。
    pub fn spawn(creds: Credentials) -> Self {
        let (cmd_tx, cmd_rx) = channel::<LoginCmd>();
        let (evt_tx, evt_rx) = channel::<LoginEvent>();

        let handle = thread::Builder::new()
            .name("miac-login".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(creds, cmd_rx, evt_tx.clone());
                }));
                if let Err(e) = r {
                    let msg = e
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| e.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "未知 panic".into());
                    eprintln!("[登录线程] 已崩溃：{msg}");
                    let _ = evt_tx.send(LoginEvent::Failed(format!("登录线程异常：{msg}")));
                }
            })
            .expect("无法启动登录线程");

        Self { tx: cmd_tx, rx: evt_rx, handle: Some(handle) }
    }

    pub fn send(&self, cmd: LoginCmd) {
        let _ = self.tx.send(cmd);
    }

    /// 非阻塞取事件（界面定时器里调用）。
    pub fn drain(&self, out: &mut Vec<LoginEvent>) {
        loop {
            match self.rx.try_recv() {
                Ok(e) => out.push(e),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
            if out.len() >= 32 {
                break;
            }
        }
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(LoginCmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
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
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().ok()?;
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
fn run(creds: Credentials, rx: Receiver<LoginCmd>, tx: Sender<LoginEvent>) {
    // 已经走完流程、正等用户选设备时，把设备列表留在这里
    let mut pending_devices: Option<(Vec<CloudDevice>, miac_core::cloud::CloudSession)> = None;
    let mut cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // 登录会话绝不发给 UI；仅在协调线程和登录流线程之间私下传递。
    let completed = std::sync::Arc::new(std::sync::Mutex::new(None::<(
        u64,
        Vec<CloudDevice>,
        miac_core::cloud::CloudSession,
    )>));
    let generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    loop {
        let cmd = match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(c) => c,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some((id, devices, session)) = completed.lock().ok().and_then(|mut s| s.take()) {
                    if id == generation.load(std::sync::atomic::Ordering::SeqCst) {
                        pending_devices = Some((devices, session));
                    }
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        // Devices 事件在 UI 侧可见前，登录流已经把敏感会话放入 completed；
        // 必须先接手它，再处理 Confirm，避免“看得到设备却不能保存”的竞态。
        if let Some((id, devices, session)) = completed.lock().ok().and_then(|mut s| s.take()) {
            if id == generation.load(std::sync::atomic::Ordering::SeqCst) {
                pending_devices = Some((devices, session));
            }
        }

        match cmd {
            LoginCmd::Shutdown => {
                cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                break;
            }
            LoginCmd::Cancel => {
                cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                pending_devices = None;
                if let Ok(mut slot) = completed.lock() {
                    *slot = None;
                }
                // 让线程回到等待下一条指令的状态（不退出，界面可能再次登录）
                let _ = tx.send(LoginEvent::Status("已取消".into()));
            }

            LoginCmd::Start => {
                // 使旧流程失效，防止它在稍后覆盖新二维码的设备会话。
                cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                let id = generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                pending_devices = None;
                if let Ok(mut slot) = completed.lock() {
                    *slot = None;
                }
                let cancel = cancel_flag.clone();
                let tx2 = tx.clone();
                let completed2 = completed.clone();
                let generation2 = generation.clone();

                // 登录本身再开一层：这样界面在等待期间仍能响应「取消」
                thread::Builder::new()
                    .name("miac-login-flow".into())
                    .stack_size(512 * 1024)
                    .spawn(move || {
                        do_login(&tx2, &cancel, &completed2, id, &generation2);
                    })
                    .ok();
            }

            LoginCmd::Confirm(idx) => {
                let Some((devices, session)) = pending_devices.take() else {
                    let _ = tx.send(LoginEvent::Failed("还没有可保存的登录会话".into()));
                    continue;
                };
                match save_credentials(&creds, &session, &devices, idx) {
                    Ok((device_name, thermo)) => {
                        let _ = tx.send(LoginEvent::Saved { device: device_name, thermometer: thermo });
                    }
                    Err(e) => {
                        // 保存失败也把会话留下，用户还能重试选设备
                        pending_devices = Some((devices, session));
                        let _ = tx.send(LoginEvent::Failed(e));
                    }
                }
            }
        }

    }
}

/// 真正的登录流程（跑在子线程里）。
fn do_login(
    tx: &Sender<LoginEvent>,
    cancel: &std::sync::atomic::AtomicBool,
    completed: &std::sync::Mutex<Option<(u64, Vec<CloudDevice>, miac_core::cloud::CloudSession)>>,
    id: u64,
    generation: &std::sync::atomic::AtomicU64,
) {
    use std::sync::atomic::Ordering;

    let _ = tx.send(LoginEvent::Status("正在向小米申请二维码…".into()));
    let mut session = login::LoginSession::new(None);

    let challenge = match session.create_challenge() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(LoginEvent::Failed(e.to_string()));
            return;
        }
    };
    let seconds = challenge.expires_at.saturating_duration_since(Instant::now()).as_secs();
    let _ = tx.send(LoginEvent::Qr { png: challenge.image.clone(), seconds });

    let result = match session.wait_for_approval(&challenge, || !cancel.load(Ordering::SeqCst)) {
        Ok(v) => v,
        Err(login::LoginError::Cancelled) => {
            let _ = tx.send(LoginEvent::Status("已取消登录".into()));
            return;
        }
        Err(e) => {
            let _ = tx.send(LoginEvent::Failed(e.to_string()));
            return;
        }
    };

    let session_data = result.to_session(miot::COUNTRY);

    let _ = tx.send(LoginEvent::Status("扫码成功，正在读取设备列表…".into()));
    let devices = match login::fetch_devices(&session_data, None) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(LoginEvent::Failed(e.to_string()));
            return;
        }
    };
    if devices.is_empty() {
        let _ = tx.send(LoginEvent::Failed("账号下没有读到任何设备".into()));
        return;
    }

    // 会话要留着（保存凭据时一起写），通过私有槽交给协调线程；已取消/过期
    // 的旧流程没有资格覆盖当前流程。
    if id != generation.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst) {
        return;
    }
    if let Ok(mut slot) = completed.lock() {
        *slot = Some((id, devices.clone(), session_data));
    }
    let _ = tx.send(LoginEvent::Devices(devices));
}

/// 写三个凭据文件。返回 (空调名, 温湿度计名)。
fn save_credentials(
    creds: &Credentials,
    session: &miac_core::cloud::CloudSession,
    devices: &[CloudDevice],
    idx: usize,
) -> Result<(String, Option<String>), String> {
    let ac = devices
        .get(idx)
        .ok_or_else(|| "所选设备不在本次登录的设备列表里".to_string())?;
    miac_core::profile::load(&ac.model, creds.primary_dir(), None)
        .map_err(|e| format!("所选设备没有可安全使用的空调规格：{e}"))?;

    creds
        .write_json(FILE_SESSION, session)
        .map_err(|e| format!("写 cloud-session.json 失败：{e}"))?;

    let device_json = serde_json::json!({
        "name": ac.name,
        "model": ac.model,
        "did": ac.did,
        "localip": ac.localip,
        "token": ac.token,
        "savedAt": login::now_iso8601(),
    });
    creds
        .write_json(FILE_DEVICE, &device_json)
        .map_err(|e| format!("写 device.json 失败：{e}"))?;

    // 顺手记下温湿度计（有才写，没有就保留原配置）
    let mut thermo_name = None;
    if let Some(t) = devices.iter().find(|d| d.is_thermometer()) {
        let tj = serde_json::json!({
            "name": t.name,
            "model": t.model,
            "did": t.did,
            "savedAt": login::now_iso8601(),
        });
        if creds.write_json(FILE_THERMOMETER, &tj).is_ok() {
            thermo_name = Some(t.name.clone());
        }
    }

    Ok((ac.name.clone(), thermo_name))
}

#[cfg(test)]
mod tests {
    use super::*;

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
