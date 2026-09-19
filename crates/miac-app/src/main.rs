#![cfg_attr(windows, windows_subsystem = "windows")]

// ─────────────────────────────────────────────────────────────────────────────
// miac-app —— 米家空调 Rust + Slint 原生版
//
// 阶段状态：
//   第 1 阶段 ✅ 内存原型（5 页 + 假数据，实测 24 MB @5min，1 进程）
//   第 2 阶段 ✅ 控制核心已接线：真实 miIO / 云端数据经工作线程进出界面
//   第 3 阶段 🔄 凭据兼容 + DPAPI 已就绪；扫码登录待做
//   第 4 阶段 🔄 五页复刻 + 明暗主题 + 右下角提示已就绪；托盘待做
//   第 5 阶段 ✅ 首启自动迁移（凭据 / 主题 / 预设）
//
// ## 线程模型
//
//   UI 线程 ──Command──▶ 工作线程（阻塞式 Controller：miIO UDP / 云端 HTTPS）
//          ◀──Event────
//
// 网络调用一律不在 UI 线程上跑：miIO 一次往返 ~0.4 秒、云端 1~2 秒，
// 直接调用会卡住重绘。用两个 channel 解耦，UI 侧只做非阻塞 try_recv。
// 这也是不引入 tokio 的前提——异步运行时会占掉数 MB 常驻内存。
//
// ## 内存自检
//
//   --probe      每 15 秒打印内存（验收：5 分钟 < 45 MB）
//   --self-test  轮换五页 100 次并检查内存增长（验收：≤ 5 MB）
//   --tour       轮询触发文件切页，供截图脚本逐页取证
// ─────────────────────────────────────────────────────────────────────────────

slint::include_modules!();

/// 系统托盘（Win32 Shell_NotifyIcon，单进程）。
mod tray;

/// 扫码登录的界面粘合层（第 3 阶段）。
mod login_ui;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};

use miac_core::controller::PropValue;
use miac_core::credentials::Credentials;
use miac_core::miot;
use miac_core::migration;
use miac_core::settings::Settings;
use miac_core::worker::{Command, DeviceSummary, Event, Worker};
use miac_core::{demo, Transport};

/// 界面上最多同时显示的提示条数（需求：最多三个）。
const MAX_TOASTS: usize = 3;
/// 每条提示存活时间。
const TOAST_TTL: Duration = Duration::from_secs(5);

/// 内部提示结构。
#[derive(Clone)]
struct Toast {
    kind: i32,
    text: String,
    born: Instant,
}

/// 应用状态。
struct App {
    ui: MainWindow,
    // 注意：Worker 刻意**不放在 App 里**。
    // 之前放在这里时踩了个坑：定时器要先 `App.borrow()` 才能取事件，取完再
    // `borrow_mut()` 处理，两步之间正好撞上 Slint 在属性变更时同步跑回调，
    // 于是 try_borrow_mut 失败、事件被丢。把 Worker 用 Rc 单独持有后，
    // 「取事件」完全不碰 App 的借用，这类重入问题从根上消失。
    creds: Credentials,
    settings: Settings,
    started: Instant,
    log: Vec<String>,
    toasts: Vec<Toast>,
    /// 当前状态快照里的属性值（属性名 → 值）
    props: Vec<(&'static str, PropValue)>,
    /// 是否已就绪
    connected: bool,
    not_ready_reason: String,
    /// 当前生效通道
    link_label: String,
    /// 上次刷新时间
    last_update: Option<Instant>,
    /// 同一时刻最多保留一个状态读取，避免云端慢时自动刷新无限积压命令。
    snapshot_in_flight: bool,
    /// 温度轮盘的本地乐观值。滚轮/拖动期间只更新画面，避免每个输入事件
    /// 都触发磁盘写入、日志重绘和网络请求。
    target_temp_override: Option<f64>,
    /// 最近一次温度输入，停止操作后再合并成一次保存/下发。
    pending_target_temp: Option<f64>,
    pending_target_changed_at: Option<Instant>,
    target_temp_write_in_flight: bool,
    /// 诊断数据
    diag: Vec<(&'static str, PropValue)>,
    /// 温湿度计
    thermometer: Option<miac_core::controller::ThermometerReading>,
    /// 自清洁/维护
    maintenance: Option<(Vec<(&'static str, PropValue)>, bool)>,
    /// 耗电日历（来自 PowerStats 事件，可能很慢，所以单独缓存）
    power: Option<miac_core::controller::PowerStats>,
    /// 原始属性读取结果
    raw_result: Option<(u16, u16, PropValue)>,
    /// 选中要读写的属性名索引（高级页）
    selected_prop: usize,
    /// 普通开关机指令的目标值，用来在成功开机后安全应用关机期间暂存的温度。
    power_on_in_flight: Option<bool>,
    /// 扫码登录线程（第 3 阶段）
    login: login_ui::LoginWorker,
    /// 登录弹层是否显示
    login_open: bool,
    /// 登录状态：0 未开始 / 1 申请中 / 2 二维码 / 3 选设备 / 4 出错
    login_state: i32,
    /// 二维码图片（解码后的 RGBA）
    login_qr: Option<slint::Image>,
    /// 弹层下的状态提示
    login_status: String,
    /// 错误文本
    login_error: String,
    /// 二维码剩余秒数
    login_seconds: u64,
    /// 二维码过期时刻
    login_expires: Option<Instant>,
    /// 扫码成功后的设备列表
    login_devices: Vec<miac_core::login::CloudDevice>,

    /// 还需要请求重绘的拍数。
    ///
    /// 用它的原因：窗口从 SW_HIDE 恢复后 Slint 不会自己重画，而 SW_SHOW 到
    /// 窗口真正被映射之间有延迟——只在恢复那一刻请求一次，帧可能画在还没
    /// 映射的表面上，结果还是空白。设成几拍，让定时器在接下来的 tick 里
    /// 各请求一次，稳过映射时机。
    repaint_ticks: u8,
}

type AppRef = Rc<RefCell<App>>;

impl App {
    fn new() -> Result<(AppRef, Rc<Worker>), slint::PlatformError> {
        let ui = MainWindow::new()?;

        // 凭据目录：与 v1 完全一致（%APPDATA%\米家空调\），并保留回退查找，
        // 这样旧版配好的凭据不用重新登录。
        let creds = Credentials::appdata();

        // ── 首次启动迁移（第 5 阶段）────────────────────────────
        let mut settings = Settings::load(&creds);
        let report = migration::run(&creds, &mut settings);

        // 凭据目录已确定，可以起两个线程了：
        //   worker 负责控制通道（读 device.json 决定局域网/云端）
        //   login  负责扫码登录（独立于控制通道，登录成功后需要重建 worker）
        let worker = Rc::new(Worker::spawn(creds.clone(), settings.clone()));
        let creds_for_login = creds.clone();

        let app = Rc::new(RefCell::new(App {
            ui,
            creds,
            settings,
            started: Instant::now(),
            log: Vec::new(),
            toasts: Vec::new(),
            props: Vec::new(),
            connected: false,
            not_ready_reason: String::new(),
            link_label: "连接中…".into(),
            last_update: None,
            snapshot_in_flight: false,
            target_temp_override: None,
            pending_target_temp: None,
            pending_target_changed_at: None,
            target_temp_write_in_flight: false,
            diag: Vec::new(),
            thermometer: None,
            maintenance: None,
            power: None,
            raw_result: None,
            selected_prop: 0,
            power_on_in_flight: None,
            repaint_ticks: 0,
            login: login_ui::LoginWorker::spawn(creds_for_login),
            login_open: false,
            login_state: 0,
            login_qr: None,
            login_status: String::new(),
            login_error: String::new(),
            login_seconds: 0,
            login_expires: None,
            login_devices: Vec::new(),
        }));

        {
            let mut a = app.borrow_mut();
            // 先把要用的值取出来，再调用 &mut self 的方法。
            // 直接写 a.add_log(a.creds.primary_dir()...) 会同时产生可变与不可变
            // 借用，编译不过——这是 Rust 借用检查最常撞的一处。
            let dir = a.creds.primary_dir().display().to_string();
            let dark = a.settings.dark;
            let transport = a.settings.transport;
            let auto = a.settings.auto_refresh;
            let encrypted = a.settings.encrypt_credentials;
            let summary = report.summary();

            a.push_static_data();
            a.add_log(format!(
                "[启动] v2 {} · Rust + Slint 原生界面 · 单进程",
                env!("CARGO_PKG_VERSION")
            ));
            a.add_log(format!("[启动] 凭据目录 {dir}"));
            a.add_log(format!("[迁移] {summary}"));
            a.ui.set_migration_summary(summary.into());
            a.ui.set_credentials_dir(dir.into());
            a.ui.set_credentials_encrypted(encrypted);
            a.ui.set_dark_theme(dark);
            a.ui.set_transport_mode(transport);
            a.ui.set_auto_refresh(auto);
            // 预设温度：迁移/设置里读到的值直接反映到界面按钮
            a.push_presets();
        }

        App::wire_callbacks(&app, worker.clone());

        // 先让界面把「未连接」状态画出来，再去连设备
        {
            let a = app.borrow();
            a.refresh_view();
        }
        worker.send(Command::Init);
        {
            let mut a = app.borrow_mut();
            a.add_log("[通道] 正在建立连接…");
        }
        Ok((app, worker))
    }

    /// 把当前状态推进界面。
    fn refresh_view(&self) {
        let ui = &self.ui;

        ui.set_dark_theme(self.settings.dark);
        ui.set_connected(self.connected);
        ui.set_not_ready_reason(self.not_ready_reason.clone().into());
        ui.set_transport_text(self.link_label.clone().into());
        ui.set_transport_state(if self.connected { 1 } else { 0 });
        ui.set_clock(clock_text().into());
        ui.set_log_text(self.log.join("\n").into());
        ui.set_updated_at(
            match self.last_update {
                Some(t) => format!("{} 秒前更新", t.elapsed().as_secs()),
                None => "尚未读取".to_string(),
            }
            .into(),
        );

        // 提示条
        let items: Vec<ToastItem> = self
            .toasts
            .iter()
            .enumerate()
            .map(|(i, t)| ToastItem {
                kind: t.kind,
                text: t.text.clone().into(),
                seq: i as i32,
            })
            .collect();
        ui.set_toasts(ModelRc::new(VecModel::from(items)));

        // ── 控制台：从快照取真实值 ─────────────────────────────
        let on = self.prop_bool("on").unwrap_or(false);
        let target = self.target_temp_override.unwrap_or_else(|| {
            if on {
                self.prop_f64("targetTemp").unwrap_or(26.0)
            } else {
                self.settings
                    .pending_temp
                    .or_else(|| self.prop_f64("targetTemp"))
                    .unwrap_or(26.0)
            }
        });
        ui.set_power_on(on);
        ui.set_power_text(if on { "运行中" } else { "已关机" }.into());
        ui.set_target_temp(target as f32);
        ui.set_preset_a(self.settings.preset(0).unwrap_or(27.5) as f32);
        ui.set_preset_b(self.settings.preset(1).unwrap_or(27.0) as f32);
        ui.set_preset_c(self.settings.preset(2).unwrap_or(26.5) as f32);
        ui.set_room_temp(fmt_or(self.prop_f64("roomTemp"), |v| format!("{v:.1}")));
        ui.set_energy_total(fmt_or(self.prop_f64("electricity"), |v| format!("{v:.1}")));

        // 温湿度计
        match self.thermometer.as_ref().filter(|t| t.available) {
            Some(t) => {
                ui.set_sensor_temp(
                    t.temperature.map(|v| format!("{v:.1}")).unwrap_or_else(|| "--".into()).into(),
                );
                ui.set_sensor_hum(
                    t.humidity.map(|v| format!("{v:.0}")).unwrap_or_else(|| "--".into()).into(),
                );
                ui.set_battery_text(
                    t.battery
                        .map(|b| format!("电量 {b:.0}%"))
                        .unwrap_or_default()
                        .into(),
                );
            }
            None => {
                let reason = self
                    .thermometer
                    .as_ref()
                    .and_then(|t| t.reason.clone())
                    .unwrap_or_else(|| "等待读取".into());
                ui.set_sensor_temp("--".into());
                ui.set_sensor_hum("--".into());
                ui.set_battery_text(reason.into());
            }
        }

        // 控制项
        ui.set_mode(self.prop_i64("mode").unwrap_or(0) as i32);
        ui.set_fan(self.prop_i64("fanLevel").unwrap_or(0) as i32);
        ui.set_wind(self.prop_i64("windDirection").unwrap_or(0) as i32);
        ui.set_vpos(self.prop_i64("verticalPos").unwrap_or(0) as i32);
        ui.set_hpos(self.prop_i64("horizontalPos").unwrap_or(0) as i32);
        ui.set_vswing(self.prop_bool("verticalSwing").unwrap_or(false));
        ui.set_hswing(self.prop_bool("horizontalSwing").unwrap_or(false));
        ui.set_eco(self.prop_bool("eco").unwrap_or(false));
        ui.set_sleep(self.prop_bool("sleep").unwrap_or(false));
        ui.set_heater(self.prop_bool("heater").unwrap_or(false));
        ui.set_dryer(self.prop_bool("dryer").unwrap_or(false));
        ui.set_favorite(self.prop_bool("favoriteOn").unwrap_or(false));
        ui.set_wind_on(self.prop_bool("windSensation").unwrap_or(false));
        ui.set_light_on(self.prop_bool("light").unwrap_or(false));
        ui.set_buzzer_on(self.prop_bool("buzzer").unwrap_or(false));
        ui.set_bright(self.prop_i64("lightBright").unwrap_or(0) as i32);
        ui.set_effect_cool(self.prop_i64("coolingEffect").unwrap_or(0) as i32);
        ui.set_effect_heat(self.prop_i64("heatingEffect").unwrap_or(0) as i32);

        // 诊断页
        let labels: Vec<slint::SharedString> = self
            .diag
            .iter()
            .map(|(n, _)| slint::SharedString::from(*n))
            .collect();
        let values: Vec<slint::SharedString> = self
            .diag
            .iter()
            .map(|(_, v)| slint::SharedString::from(v.display()))
            .collect();
        ui.set_diag_labels(ModelRc::new(VecModel::from(labels)));
        ui.set_diag_values(ModelRc::new(VecModel::from(values)));

        if let Some((values, cleaning)) = self.maintenance.as_ref() {
            let get = |name: &str| {
                values
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.display())
                    .unwrap_or_else(|| "--".into())
            };
            ui.set_clean_text(if *cleaning { "运行中".into() } else { "未运行".into() });
            ui.set_examine_text(get("examine").into());
            ui.set_run_text(format!("{} 小时", get("runDuration")).into());
        }

        // 故障
        let fault = miot::fault_info(self.prop_i64("faultValue"));
        ui.set_fault_badge(fault.badge.clone().unwrap_or_else(|| "—".into()).into());
        ui.set_fault_text(fault.text.clone().into());
        ui.set_fault_clear(fault.clear);

        // 扫码登录弹层（第 3 阶段）
        ui.set_login_open(self.login_open);
        ui.set_login_state(self.login_state);
        ui.set_login_status(self.login_status.clone().into());
        ui.set_login_error(self.login_error.clone().into());
        ui.set_login_seconds(self.login_seconds as i32);
        if let Some(img) = self.login_qr.as_ref() {
            ui.set_login_qr(img.clone());
        }
        let devs: Vec<LoginDevice> = self
            .login_devices
            .iter()
            .map(|d| LoginDevice {
                name: d.name.clone().into(),
                model: d.model.clone().into(),
                did: d.did.clone().into(),
                detail: format!(
                    "{}{}",
                    d.localip.clone().unwrap_or_else(|| "无局域网IP".into()),
                    if d.token.is_some() { " · 有token" } else { " · 无token" }
                )
                .into(),
            })
            .collect();
        ui.set_login_devices(ModelRc::new(VecModel::from(devs)));

        // 高级页：全部状态属性
        let all: Vec<slint::SharedString> = self
            .props
            .iter()
            .map(|(n, v)| slint::SharedString::from(format!("{n} = {}", v.display())))
            .collect();
        ui.set_all_props(ModelRc::new(VecModel::from(all)));

        // 高级页：原始属性地址与结果
        let names = demo::prop_names();
        let selected = names.get(self.selected_prop).cloned().unwrap_or_default();
        if let Some((siid, piid)) = miot::prop_addr(&selected) {
            ui.set_raw_addr(format!("{siid}.{piid}").into());
        }
        if let Some((siid, piid, v)) = self.raw_result.as_ref() {
            ui.set_raw_out(format!("{siid}.{piid} = {}", v.display()).into());
        }

        // 电量页（数据由 PowerStats 事件填充）
        self.push_power();
    }

    /// 设备当前是否开机（快照里取；没读到就当关机）。
    fn state_on(&self) -> bool {
        self.prop_bool("on").unwrap_or(false)
    }

    /// 属性查询小工具。
    fn prop(&self, name: &str) -> Option<&PropValue> {
        self.props.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }
    fn prop_bool(&self, name: &str) -> Option<bool> {
        self.prop(name).and_then(PropValue::as_bool)
    }
    fn prop_f64(&self, name: &str) -> Option<f64> {
        self.prop(name).and_then(PropValue::as_f64)
    }
    fn prop_i64(&self, name: &str) -> Option<i64> {
        self.prop(name).and_then(PropValue::as_i64)
    }

    /// 把设置里的预设温度反映到界面按钮上。
    fn push_presets(&mut self) {
        self.ui.set_preset_a(self.settings.preset(0).unwrap_or(27.5) as f32);
        self.ui.set_preset_b(self.settings.preset(1).unwrap_or(27.0) as f32);
        self.ui.set_preset_c(self.settings.preset(2).unwrap_or(26.5) as f32);
    }

    /// 空调关机时只在本地记录目标温度，成功开机后再由事件链下发。
    fn save_pending_temp(&mut self, temp: f64) {
        let value = ((temp * 2.0).round() / 2.0).clamp(16.0, 31.0);
        self.settings.pending_temp = Some(value);
        if let Err(e) = self.settings.save(&self.creds) {
            self.toast(3, format!("保存待应用温度失败：{e}"));
        }
        self.ui.set_target_temp(value as f32);
    }

    fn request_snapshot(&mut self, worker: &Rc<Worker>) {
        if !self.snapshot_in_flight {
            self.snapshot_in_flight = true;
            worker.send(Command::Snapshot);
        }
    }

    fn add_log(&mut self, line: impl Into<String>) {
        let line: String = line.into();
        let stamp = self.started.elapsed().as_secs();
        self.log.push(format!("[{stamp:>4}s] {line}"));
        if self.log.len() > 200 {
            let drop = self.log.len() - 200;
            self.log.drain(0..drop);
        }
    }

    /// 加一条提示。超过 3 条就丢掉最旧的——需求明确要求「最多三个」。
    fn toast(&mut self, kind: i32, text: impl Into<String>) {
        let text = text.into();
        self.add_log(format!("[提示] {text}"));
        // 同一条内容重复出现时，只刷新它的存活时间，不再叠一条
        if let Some(existing) = self.toasts.iter_mut().find(|t| t.text == text) {
            existing.born = Instant::now();
            existing.kind = kind;
            return;
        }
        self.toasts.push(Toast { kind, text, born: Instant::now() });
        while self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0);
        }
    }

    /// 清理过期提示。
    fn expire_toasts(&mut self) {
        let ttl = TOAST_TTL;
        self.toasts.retain(|t| t.born.elapsed() < ttl);
    }

    // ── 工作线程事件处理 ────────────────────────────────────────

    fn handle_events(&mut self, events: Vec<Event>, worker: &Rc<Worker>) {
        for e in events {
            match e {
                Event::Ready { link, device } => {
                    self.connected = true;
                    self.not_ready_reason.clear();
                    self.link_label = link.label().to_string();
                    self.push_device_info(&device);
                    self.add_log(format!("[通道] 已就绪：{}", link.label()));
                    // 连上就立刻拉一遍数据
                    self.request_snapshot(worker);
                    worker.send(Command::Diag);
                    worker.send(Command::Thermometer);
                    worker.send(Command::Maintenance);
                    worker.send(Command::PowerStats);
                }
                Event::NotReady { reason } => {
                    self.connected = false;
                    self.link_label = "未连接".into();
                    self.not_ready_reason = reason.clone();
                    self.add_log(format!("[通道] 未就绪：{reason}"));
                    self.toast(2, format!("未连接：{reason}"));
                }
                Event::Snapshot(s) => {
                    self.snapshot_in_flight = false;
                    self.last_update = Some(Instant::now());
                    if let Some(link) = s.link {
                        self.link_label = link.label().to_string();
                    }
                    self.props = s.status.clone();
                    self.fault_from_snapshot(&s);
                    // 只有设备回读值已经追上本地乐观值时，才解除覆盖；
                    // 如果用户在网络写入期间又调了温度，则继续保持最新值。
                    if self.pending_target_temp.is_none() {
                        if let (Some(local), Some(actual)) =
                            (self.target_temp_override, self.prop_f64("targetTemp"))
                        {
                            if (local - actual).abs() <= 0.25 {
                                self.target_temp_override = None;
                                self.target_temp_write_in_flight = false;
                            }
                        }
                    }
                }
                Event::Diag(v) => {
                    self.diag = v;
                }
                Event::Thermometer(t) => {
                    if !t.available {
                        if let Some(r) = &t.reason {
                            self.add_log(format!("[温湿度计] 不可用：{r}"));
                        }
                    }
                    self.thermometer = Some(t);
                }
                Event::PowerStats(p) => {
                    self.power = Some(*p);
                }
                Event::Maintenance { values, cleaning } => {
                    self.maintenance = Some((values, cleaning));
                }
                Event::RawValue { siid, piid, value } => {
                    self.raw_result = Some((siid, piid, value.clone()));
                    self.toast(0, format!("{siid}.{piid} = {}", value.display()));
                }
                Event::Wrote { name, ok, error } => {
                    if ok {
                        self.toast(1, format!("已下发 {name}"));
                        if name == "on" {
                            let turned_on = self.power_on_in_flight.take() == Some(true);
                            if turned_on {
                                if let Some(temp) = self.settings.pending_temp {
                                    self.add_log(format!("[预设] 开机成功，应用待处理温度 {temp:.1} ℃"));
                                    worker.send(Command::ApplyPreset(temp));
                                    continue;
                                }
                            }
                        }
                        if name == "targetTemp" {
                            self.target_temp_write_in_flight = false;
                        }
                        if name == "targetTemp" && self.settings.pending_temp.take().is_some() {
                            let _ = self.settings.save(&self.creds);
                        }
                        // 写完立刻重读，让界面显示设备真实状态而不是本地乐观值
                        self.request_snapshot(worker);
                    } else {
                        if name == "targetTemp" {
                            self.target_temp_write_in_flight = false;
                        }
                        let msg = error.unwrap_or_else(|| "未知错误".into());
                        self.toast(3, format!("下发 {name} 失败：{msg}"));
                    }
                }
                Event::Failed { op, error } => {
                    if op == "读状态" {
                        self.snapshot_in_flight = false;
                    }
                    self.toast(3, format!("{op} 失败：{error}"));
                }
                Event::CredentialsEncrypted { ok, error } => {
                    if ok {
                        self.settings.encrypt_credentials = true;
                        self.ui.set_credentials_encrypted(true);
                        if let Err(e) = self.settings.save(&self.creds) {
                            self.toast(3, format!("保存加密设置失败：{e}"));
                        } else {
                            self.toast(1, "凭据已改为 DPAPI 加密存储");
                        }
                    } else {
                        self.toast(3, format!(
                            "凭据加密失败：{}",
                            error.unwrap_or_else(|| "未知错误".into())
                        ));
                    }
                }
                Event::Log(line) => {
                    self.add_log(line);
                }
            }
        }
    }

    /// 处理登录线程事件。
    fn handle_login_events(&mut self, events: Vec<login_ui::LoginEvent>, worker: &Rc<Worker>) {
        for e in events {
            match e {
                login_ui::LoginEvent::Qr { png, seconds } => {
                    match login_ui::png_to_rgba(&png) {
                        Some(img) => {
                            self.login_qr = Some(img);
                            self.login_state = 2;
                            self.login_status = "等待在米家 App 中确认…".into();
                            self.login_seconds = seconds;
                            self.login_expires =
                                Some(Instant::now() + Duration::from_secs(seconds));
                            self.add_log(format!("[登录] 二维码已就绪，{seconds} 秒内有效"));
                        }
                        None => {
                            self.login_state = 4;
                            self.login_error = "二维码图片解码失败".into();
                        }
                    }
                }
                login_ui::LoginEvent::Devices(devs) => {
                    self.add_log(format!("[登录] 账号下读到 {} 个设备", devs.len()));
                    self.login_devices = devs;
                    self.login_state = 3;
                    self.login_status = "请选择要控制的设备".into();
                }
                login_ui::LoginEvent::Saved { device, thermometer } => {
                    self.login_state = 0;
                    self.login_open = false;
                    self.login_qr = None;
                    self.login_status.clear();
                    let extra = thermometer
                        .map(|t| format!("，同时记录了温湿度计「{t}」"))
                        .unwrap_or_default();
                    self.add_log(format!("[登录] 已保存设备「{device}」{extra}"));
                    self.toast(1, format!("登录成功：{device}{extra}"));
                    // 凭据变了 → 让工作线程重新初始化通道
                    worker.send(Command::Init);
                }
                login_ui::LoginEvent::Status(s) => {
                    self.login_status = s.clone();
                    self.add_log(format!("[登录] {s}"));
                }
                login_ui::LoginEvent::Failed(err) => {
                    self.login_state = 4;
                    self.login_error = err.clone();
                    self.add_log(format!("[登录] 失败：{err}"));
                }
            }
        }
    }

    fn fault_from_snapshot(&mut self, s: &miac_core::controller::Snapshot) {
        // 故障信息在 refresh_view 里由 faultValue 统一算，这里只记日志
        let _ = s;
    }

    fn push_device_info(&self, d: &DeviceSummary) {
        let mut lines = Vec::new();
        lines.push(format!(
            "名称：{}",
            if d.name.is_empty() { "—" } else { &d.name }
        ));
        lines.push(format!(
            "型号：{}{}",
            if d.model.is_empty() { "—" } else { &d.model },
            if d.model_matches { "" } else { "（⚠ 与目标机型不一致）" }
        ));
        lines.push(format!(
            "did：{}",
            if d.did.is_empty() { "—" } else { &d.did }
        ));
        lines.push(format!(
            "局域网：{} · token {}",
            d.localip.clone().unwrap_or_else(|| "无".into()),
            if d.has_token { "已保存" } else { "缺失" }
        ));
        lines.push(format!(
            "云端会话：{} · 温湿度计：{}",
            if d.has_cloud_session { "可用" } else { "不可用" },
            if d.has_thermometer { "已配置" } else { "未配置" }
        ));
        if let Some(t) = &d.saved_at {
            lines.push(format!("凭据写入时间：{t}"));
        }
        self.ui.set_device_info_text(lines.join("\n").into());
    }

    // ── 假数据/静态内容 ─────────────────────────────────────────

    fn push_static_data(&mut self) {
        let names: Vec<slint::SharedString> =
            demo::prop_names().into_iter().map(Into::into).collect();
        self.ui.set_prop_name_list(ModelRc::new(VecModel::from(names)));

        self.ui.set_about_text(
            format!(
                "米家空调 v{} —— Rust + Slint 原生界面\n\
                 单进程，无 Electron / Chromium / Node.js\n\
                 渲染后端：Slint software renderer\n\
                 控制核心：miIO 局域网直连 + 小米云 RPC 双通道\n\
                 凭据：兼容旧版 JSON，可选 Windows DPAPI 加密",
                env!("CARGO_PKG_VERSION")
            )
            .into(),
        );
        self.ui.set_raw_out("选择一个属性后点「读取」。".into());
    }

    /// 电量页（真实数据来自 PowerStats 事件；没有则显示占位）。
    fn push_power(&self) {
        let ui = &self.ui;
        let Some(p) = self.power.as_ref() else {
            ui.set_p_today("--".into());
            ui.set_p_month("--".into());
            ui.set_p_year("--".into());
            ui.set_p_today_h("--".into());
            ui.set_p_month_h("--".into());
            ui.set_cal_title("电量使用".into());
            ui.set_cal_energy_milli(ModelRc::new(VecModel::from(Vec::<i32>::new())));
            return;
        };

        ui.set_p_today(format!("{:.1}", p.today_energy).into());
        ui.set_p_month(format!("{:.1}", p.month_energy).into());
        ui.set_p_year(format!("{:.1}", p.year_energy).into());
        ui.set_p_today_h(format!("{:.1}", p.today_minutes as f64 / 60.0).into());
        ui.set_p_month_h(format!("{:.1}", p.month_minutes as f64 / 60.0).into());
        ui.set_cal_title(format!("{} 年 {} 月电量使用", p.year, p.month).into());
        ui.set_cal_first_weekday(p.first_weekday as i32);
        ui.set_cal_days(p.days_in_month as i32);
        ui.set_cal_today(p.day as i32);

        // 日历格：按「日」填，电量 ×1000 存整数
        let mut cells = vec![0i32; 31];
        for (day, energy, _) in &p.daily {
            let idx = (*day as usize).saturating_sub(1);
            if idx < cells.len() {
                cells[idx] = (energy * 1000.0).round() as i32;
            }
        }
        ui.set_cal_max_milli(cells.iter().copied().max().unwrap_or(1).max(1));
        ui.set_cal_energy_milli(ModelRc::new(VecModel::from(cells)));

        let months: Vec<f32> = p.months.iter().map(|(_, e, _)| *e as f32).collect();
        ui.set_month_energy(ModelRc::new(VecModel::from(months)));
    }

    // ── 回调接线 ────────────────────────────────────────────────

    fn wire_callbacks(app: &AppRef, worker: Rc<Worker>) {
        // 每个回调都写成显式闭包，并各自 `let wk = worker.clone();`。
        //
        // 为什么不用 macro_rules! 生成样板（试过，放弃了）：
        //   · 闭包外只 clone 一次 → 第一个 move 闭包把它移走，后面报 E0382；
        //   · 捕获 &Rc<Worker>   → Slint 回调要求 'static，报 does not live long enough；
        //   · 宏内注入同名绑定    → 宏体语句与闭包体对名字的解析对不上，
        //                          报 cannot find value `wk`。
        // 展开写只多两行，但一眼能看懂、编译器也不会绕晕。
        let ui = app.borrow().ui.clone_strong();

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_nav_to(move |v: i32| {
                let a = &mut *app.borrow_mut();
                a.ui.set_view(v);
                match v {
                    1 => {
                        if a.power.is_none() {
                            wk.send(Command::PowerStats);
                        }
                    }
                    2 => {
                        wk.send(Command::Diag);
                        wk.send(Command::Maintenance);
                        wk.send(Command::Thermometer);
                    }
                    _ => {}
                }
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let _wk = worker.clone();
            ui.on_toggle_theme(move || {
                let a = &mut *app.borrow_mut();
                a.settings.dark = !a.settings.dark;
                let label = if a.settings.dark { "深色" } else { "浅色" };
                a.add_log(format!("[设置] 主题 → {label}"));
                if let Err(e) = a.settings.save(&a.creds) {
                    eprintln!("[设置] 保存主题失败：{e}");
                }
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_toggle_power(move || {
                let a = &mut *app.borrow_mut();
                if !a.connected {
                    a.toast(2, "尚未连接设备，无法开关机");
                    return;
                }
                let next = !a.prop_bool("on").unwrap_or(false);
                a.add_log(format!("[操作] 电源 → {}", if next { "开机" } else { "关机" }));
                a.power_on_in_flight = Some(next);
                wk.send(Command::WriteProp {
                    name: "on".into(),
                    value: serde_json::Value::Bool(next),
                });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            ui.on_set_target_temp(move |t: f32| {
                let a = &mut *app.borrow_mut();
                // 设备限制 16~31、步长 0.5：先取整再夹取
                let v = ((t as f64 * 2.0).round() / 2.0).clamp(16.0, 31.0);
                if !a.connected {
                    // 拖动/滚轮会连续触发事件，未连接时只提示一次，
                    // 避免提示条和日志也被输入事件刷屏。
                    if a.pending_target_temp.is_none() {
                        a.toast(2, "尚未连接设备，无法调温");
                    }
                    return;
                }

                // 先只改 UI，保证轮盘 60fps 跟手；实际保存/下发交给
                // 200ms 定时器在停止输入 300ms 后合并处理。
                a.ui.set_target_temp(v as f32);
                a.target_temp_override = Some(v);
                a.pending_target_temp = Some(v);
                a.pending_target_changed_at = Some(Instant::now());
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_mode(move |m: i32| {
                let a = &mut *app.borrow_mut();
                a.ui.set_mode(m);
                wk.send(Command::WriteProp { name: "mode".into(), value: serde_json::json!(m) });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_fan(move |f: i32| {
                let a = &mut *app.borrow_mut();
                a.ui.set_fan(f);
                wk.send(Command::WriteProp { name: "fanLevel".into(), value: serde_json::json!(f) });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_wind(move |w: i32| {
                let a = &mut *app.borrow_mut();
                a.ui.set_wind(w);
                wk.send(Command::WriteProp { name: "windDirection".into(), value: serde_json::json!(w) });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_vpos(move |p: i32| {
                let a = &mut *app.borrow_mut();
                a.ui.set_vpos(p);
                wk.send(Command::WriteProp { name: "verticalPos".into(), value: serde_json::json!(p) });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_hpos(move |p: i32| {
                let a = &mut *app.borrow_mut();
                a.ui.set_hpos(p);
                wk.send(Command::WriteProp { name: "horizontalPos".into(), value: serde_json::json!(p) });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_preset(move |t: f32| {
                let a = &mut *app.borrow_mut();
                if !a.state_on() {
                    a.save_pending_temp(t as f64);
                    a.add_log(format!("[操作] 温度预设 {t:.1} ℃已暂存，开机后自动应用"));
                } else {
                    a.add_log(format!("[操作] 温度预设 {t:.1} ℃（立即下发）"));
                    wk.send(Command::ApplyPreset(t as f64));
                }
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_transport(move |t: i32| {
                let a = &mut *app.borrow_mut();
                a.settings.transport = t;
                a.ui.set_transport_mode(t);
                let name = Transport::from_index(t).label();
                a.add_log(format!("[设置] 通信通道 → {name}"));
                if let Err(e) = a.settings.save(&a.creds) {
                    eprintln!("[设置] 保存通道失败：{e}");
                }
                a.connected = false;
                a.link_label = "切换通道中…".into();
                wk.send(Command::SetTransport(Transport::from_index(t)));
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let _wk = worker.clone();
            ui.on_set_pview(move |p: i32| {
                let a = &mut *app.borrow_mut();
                a.ui.set_pview(p);
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_bright(move |b: i32| {
                let a = &mut *app.borrow_mut();
                a.ui.set_bright(b);
                wk.send(Command::WriteProp { name: "lightBright".into(), value: serde_json::json!(b) });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let _wk = worker.clone();
            ui.on_toggle_auto_refresh(move |v: bool| {
                let a = &mut *app.borrow_mut();
                a.settings.auto_refresh = v;
                a.ui.set_auto_refresh(v);
                a.add_log(format!("[设置] 自动更新 → {v}"));
                if let Err(e) = a.settings.save(&a.creds) {
                    eprintln!("[设置] 保存自动更新失败：{e}");
                }
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_toggle(move |key, v| {
                let a = &mut *app.borrow_mut();
                let key = key.to_string();
                match key.as_str() {
                    "verticalSwing" => a.ui.set_vswing(v),
                    "horizontalSwing" => a.ui.set_hswing(v),
                    "eco" => a.ui.set_eco(v),
                    "sleep" => a.ui.set_sleep(v),
                    "heater" => a.ui.set_heater(v),
                    "dryer" => a.ui.set_dryer(v),
                    "favoriteOn" => a.ui.set_favorite(v),
                    "windSensation" => a.ui.set_wind_on(v),
                    "light" => a.ui.set_light_on(v),
                    "buzzer" => a.ui.set_buzzer_on(v),
                    other => {
                        a.toast(2, format!("未知开关属性 {other}"));
                        return;
                    }
                }
                a.add_log(format!("[操作] {key} → {v}"));
                wk.send(Command::WriteProp { name: key, value: serde_json::Value::Bool(v) });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_set_effect(move |which, level| {
                let a = &mut *app.borrow_mut();
                let which = which.to_string();
                let name = if which == "cool" { "coolingEffect" } else { "heatingEffect" };
                if which == "cool" {
                    a.ui.set_effect_cool(level);
                } else {
                    a.ui.set_effect_heat(level);
                }
                a.add_log(format!("[操作] {name} → {level}"));
                wk.send(Command::WriteProp { name: name.into(), value: serde_json::json!(level) });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_raw_read(move || {
                let a = &mut *app.borrow_mut();
                let names = demo::prop_names();
                let selected = names.get(a.selected_prop).cloned().unwrap_or_default();
                match miot::prop_addr(&selected) {
                    Some((siid, piid)) => {
                        a.add_log(format!("[操作] 读取原始属性 {selected}（{siid}.{piid}）"));
                        wk.send(Command::RawRead { siid, piid });
                    }
                    None => a.toast(2, "请先选择一个属性"),
                }
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_raw_write(move || {
                let a = &mut *app.borrow_mut();
                let names = demo::prop_names();
                let selected = names.get(a.selected_prop).cloned().unwrap_or_default();
                let text = a.ui.get_raw_value().to_string();
                let Some((siid, piid)) = miot::prop_addr(&selected) else {
                    a.toast(2, "请先选择一个属性");
                    return;
                };
                // 安全边界：只允许写本机型已登记的属性，且值必须是 bool 或数字
                let value: serde_json::Value = if text.eq_ignore_ascii_case("true") {
                    serde_json::Value::Bool(true)
                } else if text.eq_ignore_ascii_case("false") {
                    serde_json::Value::Bool(false)
                } else {
                    match text.trim().parse::<f64>() {
                        Ok(n) => serde_json::json!(n),
                        Err(_) => {
                            a.toast(2, "写入值需为 true / false 或数字");
                            return;
                        }
                    }
                };
                a.add_log(format!("[操作] 写入 {selected}={value}"));
                wk.send(Command::RawWrite { siid, piid, value });
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let _wk = worker.clone();
            ui.on_clear_log(move || {
                let a = &mut *app.borrow_mut();
                a.log.clear();
                a.add_log("[日志] 已清空");
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_refresh(move || {
                let a = &mut *app.borrow_mut();
                if !a.connected {
                    a.toast(2, "尚未连接设备");
                    return;
                }
                a.add_log("[操作] 手动刷新状态");
                a.request_snapshot(&wk);
                wk.send(Command::Diag);
                wk.send(Command::Thermometer);
                wk.send(Command::Maintenance);
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let wk = worker.clone();
            ui.on_encrypt_credentials(move || {
                let a = &mut *app.borrow_mut();
                if a.settings.encrypt_credentials {
                    a.toast(0, "凭据已经是 DPAPI 加密存储");
                    return;
                }
                wk.send(Command::EncryptCredentials);
                a.toast(0, "正在使用 DPAPI 加密凭据…");
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            ui.on_select_prop(move |index: i32| {
                let a = &mut *app.borrow_mut();
                if index >= 0 && (index as usize) < demo::prop_names().len() {
                    a.selected_prop = index as usize;
                    a.ui.set_selected_prop(index);
                    a.refresh_view();
                }
            });
        }

        // ── 扫码登录回调（第 3 阶段）──
        {
            let app = app.clone();
            ui.on_login_start(move || {
                let mut a = app.borrow_mut();
                a.login_open = true;
                a.login_state = 1;
                a.login_error.clear();
                a.login_status = "正在向小米申请二维码…".into();
                a.login_qr = None;
                a.login_devices.clear();
                a.add_log("[登录] 开始扫码登录");
                a.login.send(login_ui::LoginCmd::Start);
                a.refresh_view();
            });
        }
        {
            let app = app.clone();
            ui.on_login_cancel(move || {
                let mut a = app.borrow_mut();
                a.login_open = false;
                a.login_state = 0;
                a.login_error.clear();
                a.login_status.clear();
                a.login_qr = None;
                a.login_devices.clear();
                a.login.send(login_ui::LoginCmd::Cancel);
                a.add_log("[登录] 已取消");
                a.refresh_view();
            });
        }
        {
            let app = app.clone();
            ui.on_login_pick(move |i: i32| {
                let a = &mut *app.borrow_mut();
                if (i as usize) < a.login_devices.len() {
                    a.ui.set_login_selected(i);
                }
            });
        }
        {
            let app = app.clone();
            ui.on_login_confirm(move || {
                let mut a = app.borrow_mut();
                let idx = a.ui.get_login_selected() as usize;
                if idx >= a.login_devices.len() {
                    a.toast(2, "请先选择一个设备");
                    a.refresh_view();
                    return;
                }
                let name = a.login_devices[idx].name.clone();
                a.login_status = "正在保存凭据…".into();
                a.add_log(format!("[登录] 选用设备「{name}」"));
                a.login.send(login_ui::LoginCmd::Confirm(idx));
                a.refresh_view();
            });
        }

        {
            let app = app.clone();
            let _wk = worker.clone();
            ui.on_dismiss_toast(move |i: i32| {
                let a = &mut *app.borrow_mut();
                let idx = i as usize;
                if idx < a.toasts.len() {
                    a.toasts.remove(idx);
                }
                a.refresh_view();
            });
        }
    }
}

/// 有值就格式化，没有就 "--"。
fn fmt_or(v: Option<f64>, f: impl Fn(f64) -> String) -> slint::SharedString {
    v.map(f).unwrap_or_else(|| "--".into()).into()
}

/// Unix 秒 → HH:MM（用系统时区偏移换算）。
fn clock_text() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let total = secs as i64 + local_offset_minutes() as i64 * 60;
    let day = total.rem_euclid(86400);
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

/// 本地时区相对 UTC 的偏移（分钟）。
fn local_offset_minutes() -> i32 {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Time::{
            GetTimeZoneInformation, TIME_ZONE_INFORMATION,
        };
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let ret = GetTimeZoneInformation(&mut tz);
        let bias = if ret == 2 {
            tz.Bias + tz.DaylightBias
        } else {
            tz.Bias + tz.StandardBias
        };
        -bias
    }
    #[cfg(not(windows))]
    {
        480
    }
}

#[cfg(windows)]
thread_local! {
    /// 枚举窗口时的临时落点（EnumWindows 的回调拿不到闭包捕获）。
    static FOUND_HWND: std::cell::Cell<Option<windows_sys::Win32::Foundation::HWND>> =
        const { std::cell::Cell::new(None) };
}

/// 按窗口标题找到主窗口句柄。
///
/// Slint 不对外暴露 HWND（没有重导出 raw-window-handle），而托盘要
/// 「左键切回窗口」「关闭时隐藏窗口」都需要它。这里用标题枚举来找，
/// 是当前依赖条件下最直接的拿到 HWND 的办法。
#[cfg(windows)]
fn find_main_hwnd(title: &str) -> windows_sys::Win32::Foundation::HWND {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW,
    };

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let want = &*(lparam as *const String);
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return 1; // 继续枚举
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == *want {
            FOUND_HWND.with(|f| f.set(Some(hwnd)));
            return 0; // 找到了，停止
        }
        1
    }

    FOUND_HWND.with(|f| f.set(None));
    let target = title.to_string();
    unsafe {
        EnumWindows(Some(cb), &target as *const String as LPARAM);
    }
    FOUND_HWND
        .with(|f| f.get())
        .unwrap_or(std::ptr::null_mut())
}

/// 取当前进程的工作集与私有提交（字节）。
fn memory_bytes() -> Option<(u64, u64)> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) == 0 {
            return None;
        }
        Some((pmc.WorkingSetSize as u64, pmc.PagefileUsage as u64))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn select_backend() -> Result<(), slint::PlatformError> {
    let which = std::env::var("MIAC_BACKEND").unwrap_or_default();
    let renderer = match which.as_str() {
        "femtovg" => "femtovg",
        "wgpu" => "wgpu",
        "skia" => "skia",
        _ => "software",
    };
    if !which.is_empty() {
        eprintln!("[后端] MIAC_BACKEND={which} → renderer={renderer}");
    }
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.into())
        .select()
}

fn main() -> Result<(), slint::PlatformError> {
    let args: Vec<String> = std::env::args().collect();
    let self_test = args.iter().any(|a| a == "--self-test");
    let probe = args.iter().any(|a| a == "--probe");
    let tour = args.iter().any(|a| a == "--tour");

    select_backend()?;
    let (app, worker) = App::new()?;

    // --login：启动就打开扫码登录弹层
    // 用途：一是换账号（已连接时也能直接登录），二是让「登录界面」可被截图验证
    if std::env::args().any(|a| a == "--login") {
        {
            let mut a = app.borrow_mut();
            a.login_open = true;
            a.login_state = 0;
            a.login_status.clear();
            a.login_error.clear();
            a.refresh_view();
        }
        println!("[登录] --login：已打开扫码登录弹层");
    }

    // ── 界面轮询：收工作线程事件 + 到期提示 + 定时刷新 ────────────
    //
    // 全部跑在 UI 线程上（Slint 的 Timer 就在 UI 线程），但**不做任何阻塞调用**，
    // 所以不会卡住重绘。事件用非阻塞 try_recv 取。
    //
    // 关键设计：Worker 用 Rc 单独持有，「取事件」这一步完全不碰 App 的 RefCell。
    // 早先把 Worker 放进 App 里，需要先 borrow() 取事件、再 borrow_mut() 处理，
    // 而 Slint 会在属性变更时同步跑回调，两步之间正好被插进来，导致
    // try_borrow_mut 失败、事件被整批丢掉。现在从结构上避免了这种重入。
    let tick_worker = worker.clone();
    let tick_app = app.clone();
    let tick = Timer::default();
    let mut tick_count: u64 = 0;
    // 本拍没能处理的事件暂存在这里，下一拍补处理。
    //
    // 为什么必须有它：定时器第一拍几乎总会撞上「App 正被占用」（启动流程还没
    // 走完），而事件一旦被 try_recv 取出就离开了 Worker 的通道——直接丢掉的话，
    // 那条「通道已就绪」永远到不了界面，界面就一直停在「连接中…」。
    // 这个 bug 的表现是「能连上却不刷新」，极易误判成网络问题。
    let mut pending: Vec<Event> = Vec::new();
    // 登录事件也必须保留到 App 可变借用成功；扫码成功事件只发送一次，不能丢。
    let mut pending_login: Vec<login_ui::LoginEvent> = Vec::new();
    let probe_enabled = probe;
    let base_ws = Rc::new(std::cell::Cell::new(
        memory_bytes().map(|(w, _)| w).unwrap_or(0),
    ));
    let last_probe = Rc::new(std::cell::Cell::new(Instant::now()));

    tick.start(TimerMode::Repeated, Duration::from_millis(200), move || {
        tick_count += 1;

        // 1) 取事件——不借用 App
        let mut events = std::mem::take(&mut pending);
        tick_worker.drain(&mut events);
        // 2) 处理事件并刷新界面。
        //    用 try_borrow_mut 而不是 borrow_mut：万一某个 Slint 回调在属性变更时
        //    同步重入了本定时器，宁可这一拍不处理（事件放回 pending，不丢），
        //    也不能 panic 把程序打崩。
        if !events.is_empty() {
            match tick_app.try_borrow_mut() {
                Ok(mut a) => {
                    a.handle_events(events, &tick_worker);
                    a.refresh_view();
                }
                Err(_) => {
                    pending = events; // 下一拍（200ms 后）再处理
                    return;
                }
            }
        }

        // 2.5) 合并温度轮盘输入。
        // 轮盘事件只负责更新本地画面；停止输入 300ms 后才做一次磁盘写入
        // 或设备写入，避免快速滚轮把 UI 线程和工作线程都塞满。
        if let Ok(mut a) = tick_app.try_borrow_mut() {
            let settled_temp = a
                .pending_target_temp
                .zip(a.pending_target_changed_at)
                .filter(|(_, changed_at)| changed_at.elapsed() >= Duration::from_millis(300))
                .map(|(temp, _)| temp);

            if let Some(temp) = settled_temp {
                a.pending_target_temp = None;
                a.pending_target_changed_at = None;

                if !a.state_on() {
                    a.save_pending_temp(temp);
                    a.add_log(format!("[操作] 空调关机，已暂存 {temp:.1} ℃，开机后自动应用"));
                    a.refresh_view();
                } else if a.connected && !a.target_temp_write_in_flight {
                    a.target_temp_write_in_flight = true;
                    a.add_log(format!("[操作] 设定温度 → {temp:.1} ℃"));
                    tick_worker.send(Command::WriteProp {
                        name: "targetTemp".into(),
                        value: serde_json::json!(temp),
                    });
                    a.refresh_view();
                }
            }
        }

        // 3) 每 1 秒清理过期提示、更新时钟与「N 秒前更新」
        if tick_count % 5 == 0 {
            if let Ok(mut a) = tick_app.try_borrow_mut() {
                let before = a.toasts.len();
                a.expire_toasts();
                if a.toasts.len() != before {
                    a.refresh_view();
                } else {
                    a.ui.set_clock(clock_text().into());
                    if let Some(t) = a.last_update {
                        a.ui.set_updated_at(format!("{} 秒前更新", t.elapsed().as_secs()).into());
                    }
                }
            }
        }

        // 4) 自动刷新：按设置的间隔拉状态（默认 6 秒，与 v1 一致）
        //    200ms 一拍，所以 1 秒 = 5 拍，secs 秒 = 5*secs 拍。
        if tick_count % 5 == 0 {
            if let Ok(mut a) = tick_app.try_borrow_mut() {
                let secs = a.settings.refresh_secs.max(2);
                if a.settings.auto_refresh
                    && a.connected
                    && tick_count % (5 * secs) == 0
                {
                    a.request_snapshot(&tick_worker);
                }
            }
        }

        // 5.5) 恢复窗口后连续几拍请求重绘，确保内容真的画出来
        if let Ok(mut a) = tick_app.try_borrow_mut() {
            if a.repaint_ticks > 0 {
                a.repaint_ticks -= 1;
                a.ui.window().request_redraw();
            }
        }

        // 5.6) 登录线程事件 + 二维码倒计时
        {
            let mut levents = std::mem::take(&mut pending_login);
            if let Ok(a) = tick_app.try_borrow() {
                a.login.drain(&mut levents);
            }
            if !levents.is_empty() {
                match tick_app.try_borrow_mut() {
                    Ok(mut a) => {
                        a.handle_login_events(levents, &tick_worker);
                        a.refresh_view();
                    }
                    Err(_) => {
                        pending_login = levents;
                    }
                }
            }
            // 每拍更新剩余秒数（弹层里要显示倒计时）
            if let Ok(mut a) = tick_app.try_borrow_mut() {
                if a.login_open && a.login_state == 2 {
                    if let Some(exp) = a.login_expires {
                        let left = exp.saturating_duration_since(Instant::now()).as_secs();
                        if left != a.login_seconds {
                            a.login_seconds = left;
                            a.ui.set_login_seconds(left as i32);
                            if left == 0 {
                                a.login_state = 4;
                                a.login_error = "二维码已过期，请重新生成".into();
                                a.refresh_view();
                            }
                        }
                    }
                }
            }
        }

        // 6) 处理托盘命令（左键显示 / 右键菜单：显示 / 开关机 / 退出）
        #[cfg(windows)]
        for cmd in tray::drain_commands() {
            match cmd {
                tray::TrayCommand::Show => {
                    // 先让 **Slint 自己**把窗口显示出来，再补一次 Win32 置前。
                    //
                    // 为什么不能只用 Win32 的 ShowWindow(SW_SHOW)：实测那样恢复后
                    // 窗口是一片纯白（采样固定像素得到 #FFFFFF，正常时是深色主题）——
                    // Slint 不知道窗口又可见了，不会重画。
                    // 走 `window().show()` 是让 Slint 自己感知状态变化的正路。
                    let ok = match tick_app.try_borrow() {
                        Ok(a) => {
                            let _ = a.ui.window().show();
                            true
                        }
                        Err(_) => false,
                    };
                    tray::show_main_window();
                    if ok {
                        // 显示后窗口句柄可能被重建，重新绑定托盘与关闭拦截
                        let enable_tray = tick_app
                            .try_borrow()
                            .map(|a| a.settings.close_to_tray)
                            .unwrap_or(true);
                        let hwnd = find_main_hwnd("米家空调");
                        if !hwnd.is_null() {
                            tray::attach_main_window(hwnd, enable_tray);
                        }
                    }
                    if let Ok(mut a) = tick_app.try_borrow_mut() {
                        a.repaint_ticks = 5;
                        a.add_log("[托盘] 显示主界面");
                        a.refresh_view();
                    }
                }
                tray::TrayCommand::TogglePower => {
                    // 与界面上的电源按钮走同一条路径（写属性 → 重读快照）
                    let connected = tick_app
                        .try_borrow()
                        .map(|a| a.connected)
                        .unwrap_or(false);
                    if connected {
                        let next = tick_app
                            .try_borrow()
                            .ok()
                            .and_then(|a| a.prop_bool("on"))
                            .map(|on| !on)
                            .unwrap_or(true);
                        if let Ok(mut a) = tick_app.try_borrow_mut() {
                            a.power_on_in_flight = Some(next);
                            a.add_log(format!(
                                "[托盘] 电源 → {}",
                                if next { "开机" } else { "关机" }
                            ));
                            a.refresh_view();
                        }
                        tick_worker.send(Command::WriteProp {
                            name: "on".into(),
                            value: serde_json::Value::Bool(next),
                        });
                    } else if let Ok(mut a) = tick_app.try_borrow_mut() {
                        a.toast(2, "尚未连接设备，托盘开机/关机不可用");
                        a.refresh_view();
                    }
                }
                tray::TrayCommand::Exit => {
                    if let Ok(mut a) = tick_app.try_borrow_mut() {
                        a.add_log("[托盘] 退出程序");
                        a.refresh_view();
                    }
                    let _ = slint::quit_event_loop();
                }
            }
        }

        // 7) 每 15 秒打一行内存（验收用）
        if probe_enabled && last_probe.get().elapsed() >= Duration::from_secs(15) {
            last_probe.set(Instant::now());
            if let Some((ws, priv_bytes)) = memory_bytes() {
                let base = base_ws.get();
                if let Ok(a) = tick_app.try_borrow() {
                    println!(
                        "[内存] {:>6.1}s  工作集 {:>6.1} MB（基线 {:>6.1} MB，Δ{:+.2} MB）  私有 {:>6.1} MB  已连接={}",
                        a.started.elapsed().as_secs_f64(),
                        mb(ws),
                        mb(base),
                        mb(ws) - mb(base),
                        mb(priv_bytes),
                        a.connected,
                    );
                }
            }
        }
    });

    if self_test {
        let _tick = tick;
        return run_self_test(&app);
    }

    // --tour：轮询触发文件切页，供截图脚本确定性取证
    let tour_timer = Timer::default();
    if tour {
        let tour_app = app.clone();
        let trigger =
            std::env::var("MIAC_TOUR_FILE").unwrap_or_else(|_| "miac-tour.txt".to_string());
        println!("[巡览] 等待触发文件：{trigger}（写入 0~4 即切到对应页）");
        let mut last: i32 = -1;
        tour_timer.start(TimerMode::Repeated, Duration::from_millis(120), move || {
            let Ok(text) = std::fs::read_to_string(&trigger) else { return };
            let Ok(n) = text.trim().parse::<i32>() else { return };
            if n == last || !(0..5).contains(&n) {
                return;
            }
            last = n;
            let a = tour_app.borrow();
            a.ui.set_view(n);
            a.ui.set_clock(clock_text().into());
            drop(a);
            println!("[巡览] 已切到第 {n} 页");
        });
    }
    let _tour_timer = tour_timer;

    if probe {
        println!("[自检] 内存探针已启动：每 15 秒打印一次，关闭窗口结束。");
        println!("[自检] 验收门：运行 5 分钟后工作集 < 45 MB（硬上限 50 MB）");
    }

    // ── 系统托盘 ────────────────────────────────────────────────
    // 必须在 UI 线程创建（消息专用窗口要在同一线程，它的消息才会被
    // Slint/winit 的消息泵派发），所以放在 `ui.run()` 之前。
    // 创建失败不影响主功能：只是没有托盘图标，界面照常可用。
    #[cfg(windows)]
    let tray_handle = match tray::Tray::new("米家空调") {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("[托盘] 创建失败（不影响使用）：{e}");
            None
        }
    };

    // 窗口句柄要等 winit 真正建好窗口才拿得到，所以延后一点再绑定托盘。
    // 绑定之后：左键点托盘能切回窗口，关闭按钮能正确地收进托盘。
    #[cfg(windows)]
    let _attach_timer = {
        let attach_app = app.clone();
        let attach_timer = Rc::new(Timer::default());
        let attach_timer_cb = attach_timer.clone();
        let mut attach_tries = 0u32;
        attach_timer.start(TimerMode::Repeated, Duration::from_millis(300), move || {
            attach_tries += 1;
            let enable_tray = attach_app
                .try_borrow()
                .map(|a| a.settings.close_to_tray)
                .unwrap_or(true);
            let hwnd = find_main_hwnd("米家空调");
            if !hwnd.is_null() {
                // 绑定 + 挂钩「关闭时收进托盘」
                tray::attach_main_window(hwnd, enable_tray);
                if let Ok(mut a) = attach_app.try_borrow_mut() {
                    a.add_log("[托盘] 已就绪（右键菜单：显示 / 开机 / 关机 / 退出）");
                    a.refresh_view();
                }
                attach_timer_cb.stop(); // 绑好就不用再轮询了
            } else if attach_tries > 40 {
                eprintln!("[托盘] 未找到主窗口句柄，托盘交互不可用");
                attach_timer_cb.stop();
            }
        });
        attach_timer
    };

    // ⚠ 关键：`ui.run()` 必须在**不持有 App 借用**的情况下调用。
    //
    // 早先写成 `let code = { let a = app.borrow(); a.ui.run() };`，看着像把借用
    // 限制在块里，实际上那个 Ref 要活到 `a.ui.run()` 返回——也就是整个事件循环
    // 期间都被不可变借用占着，于是定时器第一拍 `try_borrow_mut()` 就失败，
    // 事件永远处理不了（界面一直显示「连接中…」）。
    // 正确做法：先把界面句柄 clone 出来，再在无借用状态下 run。
    let code = {
        let ui = app.borrow().ui.clone_strong();
        ui.run()
    };
    // 退出前让工作线程收尾（Wire 的 Drop 里也会做，这里显式一点）
    worker.shutdown();
    drop(tick);
    #[cfg(windows)]
    drop(tray_handle);
    let _ = app;
    code
}

/// --self-test：轮换五个页面 100 次，检查内存增长。
///
/// 基线取「预热一轮之后」的值：首次构造页面元素树会一次性抬高工作集，
/// 那是一次性成本不是泄漏（详见 README 的实测说明）。
fn run_self_test(app: &AppRef) -> Result<(), slint::PlatformError> {
    let ui = app.borrow().ui.clone_strong();
    let app2 = app.clone();

    println!("[自检] 轮换五个页面 × 20 轮（共 100 次切换）+ 每次刷新数据");
    if let Some((ws, priv_bytes)) = memory_bytes() {
        println!("[自检] 冷启动水位：工作集 {:.1} MB，私有 {:.1} MB", mb(ws), mb(priv_bytes));
    }

    let step = Rc::new(std::cell::Cell::new(0i32));
    const STEPS: i32 = 100;
    const WARMUP: i32 = 10;

    let timer = Rc::new(Timer::default());
    let timer_in_cb = timer.clone();
    let baseline: Rc<std::cell::Cell<Option<u64>>> = Rc::new(std::cell::Cell::new(None));

    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        let n = step.get() + 1;
        step.set(n);

        {
            let a = app2.borrow();
            a.ui.set_view(n % 5);
            a.refresh_view();
        }

        if n == WARMUP {
            if let Some((ws, priv_bytes)) = memory_bytes() {
                baseline.set(Some(ws));
                println!(
                    "[自检] 预热 {} 次后基线：工作集 {:.1} MB，私有 {:.1} MB",
                    WARMUP,
                    mb(ws),
                    mb(priv_bytes)
                );
            }
        }

        if n % 25 == 0 {
            if let (Some((ws, priv_bytes)), Some(base)) = (memory_bytes(), baseline.get()) {
                println!(
                    "[自检] {:>3}/{} 次切换  工作集 {:>6.1} MB（较基线 {:+.2} MB）  私有 {:>6.1} MB",
                    n,
                    STEPS,
                    mb(ws),
                    mb(ws) - mb(base),
                    mb(priv_bytes),
                );
            }
        }

        if n >= STEPS {
            timer_in_cb.stop();
            if let (Some((ws, priv_bytes)), Some(base)) = (memory_bytes(), baseline.get()) {
                let growth = mb(ws) - mb(base);
                println!(
                    "[自检] 完成：工作集 {:.1} MB（基线 {:.1} MB，增长 {:+.2} MB，限值 +5.00 MB）",
                    mb(ws),
                    mb(base),
                    growth
                );
                println!("[自检] 私有提交 {:.1} MB", mb(priv_bytes));
                println!("[自检] 结论：{}", if growth <= 5.0 { "通过" } else { "超限" });
            }
            let _ = slint::quit_event_loop();
        }
    });

    let _ = ui.run();
    Ok(())
}
