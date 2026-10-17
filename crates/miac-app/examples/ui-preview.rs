//! Offline UI preview: no credentials, device connections, or settings writes.
//! cargo run -p miac-app --example ui-preview -- /tmp/miac-ui
//! Add --interactive to explore the six pages in a native window.
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{path::PathBuf, rc::Rc};
slint::include_modules!();
#[path = "../src/window_config.rs"]
mod window_config;
#[path = "../src/theme_art.rs"]
mod theme_art;

fn strings(values: &[&str]) -> ModelRc<SharedString> {
    Rc::new(VecModel::from(
        values.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
    ))
    .into()
}

// Measure the rendered circle and marker independently. Comparing their radii
// catches automatic path fitting even when both use the same source endpoint.
fn assert_dial_marker_aligned(pixels: &slint::SharedPixelBuffer<slint::Rgb8Pixel>, scale: f64) {
    let width = pixels.width() as usize;
    let rgb_at = |x: usize, y: usize| {
        let index = (y * width + x) * 3;
        &pixels.as_bytes()[index..index + 3]
    };
    let (x0, x1) = ((460.0 * scale) as usize, (750.0 * scale) as usize);
    let (y0, y1) = ((270.0 * scale) as usize, (550.0 * scale) as usize);
    let (mut left, mut right, mut top, mut bottom) = (usize::MAX, 0, usize::MAX, 0);
    // Follow the connected disk only: antialiased ring pixels can happen to
    // equal the disk's background colour but are separated by its white gap.
    let mut visited = vec![false; (x1 - x0) * (y1 - y0)];
    let mut pending = vec![((600.0 * scale) as usize, (480.0 * scale) as usize)];
    while let Some((x, y)) = pending.pop() {
        if x < x0 || x >= x1 || y < y0 || y >= y1 { continue; }
        let index = (y - y0) * (x1 - x0) + x - x0;
        if visited[index] { continue; }
        visited[index] = true;
        if rgb_at(x, y) != [240, 244, 236] { continue; }
        left = left.min(x); right = right.max(x);
        top = top.min(y); bottom = bottom.max(y);
        pending.extend([(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]);
    }
    assert_ne!(left, usize::MAX, "dial's central disk must be visible");
    let cx = (left + right + 1) as f64 / 2.0;
    let cy = (top + bottom + 1) as f64 / 2.0;
    let (mut arc_top, mut arc_bottom) = (usize::MAX, 0);
    let (mut marker_x, mut marker_y, mut marker_count) = (0.0, 0.0, 0usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let rgb = rgb_at(x, y);
            let px = x as f64 + 0.5;
            let py = y as f64 + 0.5;
            if (px - cx).abs() < 2.0 * scale && py < cy - 100.0 * scale
                && (rgb == [132, 170, 109] || rgb == [226, 233, 218])
            {
                arc_top = arc_top.min(y); arc_bottom = arc_bottom.max(y);
            }
            let radius = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            if rgb == [54, 95, 67] && radius > 110.0 * scale && radius < 145.0 * scale {
                marker_x += px; marker_y += py; marker_count += 1;
            }
        }
    }
    assert_ne!(arc_top, usize::MAX, "continuous ring must be rendered");
    assert!(marker_count > 0, "dial marker must be rendered");
    let ring_radius = cy - (arc_top + arc_bottom + 1) as f64 / 2.0;
    let marker_radius = ((marker_x / marker_count as f64 - cx).powi(2)
        + (marker_y / marker_count as f64 - cy).powi(2)).sqrt();
    assert!((marker_radius - ring_radius).abs() <= 1.5 * scale,
        "marker must sit on the rendered ring at scale {scale}: marker radius={marker_radius}, ring radius={ring_radius}");
}

fn fixture(ui: &MainWindow) -> Rc<std::cell::RefCell<theme_art::ArtCache>> {
    let art_cache = theme_art::install(ui);
    let p = miac_core::profile::Profile::legacy();
    let choices = |key: &str| ModelRc::new(VecModel::from(p.properties[key].choices.iter().map(|(v,n)| ControlChoice { value: *v as i32, label: n.clone().into() }).collect::<Vec<_>>()));
    ui.set_mode_options(choices("mode"));
    ui.set_fan_options(choices("fanLevel"));
    ui.set_extended_controls(true);
    ui.set_can_vswing(true);
    ui.set_can_hswing(true);
    ui.set_can_eco(true);
    ui.set_can_sleep(true);
    ui.set_can_heater(true);
    ui.set_can_dryer(true);
    ui.set_can_light(true);
    ui.set_can_buzzer(true);
    ui.set_device_name("米家空调 · 离线预览".into());
    ui.set_connected(true);
    ui.set_power_known(true);
    ui.set_not_ready_reason("".into());
    ui.set_theme_index(0);
    ui.set_room_temp("29.0".into());
    ui.set_sensor_temp("25.8".into());
    ui.set_sensor_hum("42".into());
    ui.set_energy_total("41.74".into());
    ui.set_battery_text("温湿度计电量 100%".into());
    ui.set_eco(true);
    ui.set_dryer(true);
    ui.set_updated_at("示例数据 · 不连接设备".into());
    ui.set_p_today("0.62".into());
    ui.set_p_month("12.30".into());
    ui.set_p_year("41.74".into());
    ui.set_p_today_h("3.5".into());
    ui.set_p_month_h("86.5".into());
    ui.set_cal_title("2026 年 9 月".into());
    ui.set_cal_first_weekday(2);
    ui.set_cal_days(30);
    ui.set_cal_today(20);
    ui.set_cal_max_milli(930);
    ui.set_cal_energy_milli(
        Rc::new(VecModel::from(vec![
            340, 720, 650, 230, 910, 540, 340, 470, 830, 250, 600, 710, 460, 590, 840, 650, 930,
            740, 880, 620,
        ]))
        .into(),
    );
    ui.set_month_energy(
        Rc::new(VecModel::from(vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 3.0, 12.0, 14.44, 12.3, 0.0, 0.0, 0.0,
        ]))
        .into(),
    );
    ui.set_diag_labels(strings(&[
        "indoorPipeTemp/室内盘管温度",
        "indoorFanSpeed/室内风机转速",
        "outdoorTemp/室外温度",
        "outdoorPipeTemp/室外盘管温度",
        "compressorFreq/压缩机频率",
        "outdoorCurrent/室外机电流",
        "outdoorVoltage/室外机电压",
        "runDuration/累计运行时长",
    ]));
    ui.set_diag_values(strings(&[
        "23.9",
        "0",
        "13",
        "12",
        "0.0",
        "0.0",
        "225.0",
        "278.6",
    ]));
    ui.set_diag_units(strings(&["℃", "rpm", "℃", "℃", "Hz", "A", "V", "小时"]));
    ui.set_clean_text("待机".into());
    ui.set_examine_text("未运行".into());
    ui.set_run_text("128 小时".into());
    ui.set_fault_badge("正常".into());
    ui.set_fault_known(true);
    ui.set_fault_text("未检测到故障 · 示例状态".into());
    ui.set_prop_name_list(strings(&[
        "电源",
        "运行模式",
        "目标温度",
        "风速",
        "上下摆风",
    ]));
    ui.set_all_props(strings(&[
        "电源 = true",
        "运行模式 = 制冷",
        "目标温度 = 26.0°C",
        "风速 = 自动",
        "上下摆风 = 开启",
        "左右扫风 = 关闭",
        "ECO = 开启",
    ]));
    ui.set_raw_addr("2.1".into());
    ui.set_raw_value("true".into());
    ui.set_raw_out("true".into());
    ui.set_device_info_text(
        "设备名称    米家空调\n设备型号    xiaomi.airc.h53h00\n连接方式    局域网直连".into(),
    );
    ui.set_credentials_dir("%APPDATA%\\米家空调".into());
    ui.set_credentials_status_title("离线预览：未检查本机凭据".into());
    ui.set_credentials_status_detail("真实程序会自动加密新登录凭据，并在启动时升级可读的旧凭据；预览不会访问本机文件。".into());
    ui.set_migration_summary("离线预览：不读取或修改本机配置。".into());
    ui.set_log_text("14:32:06  设备状态更新成功\n14:32:00  已切换到局域网直连\n14:31:58  温湿度计已连接\n14:31:55  已载入本地设备配置".into());
    ui.set_about_text("mi / 米家空调 v2\n轻一点，也舒适一点。".into());
    let weak = ui.as_weak();
    ui.on_nav_to(move |v| weak.unwrap().set_view(v));
    let weak = ui.as_weak();
    ui.on_set_theme(move |theme| weak.unwrap().set_theme_index(theme));
    let weak = ui.as_weak();
    ui.on_set_target_temp(move |v| weak.unwrap().set_target_temp(v));
    let weak = ui.as_weak();
    ui.on_toggle_power(move || {
        let u = weak.unwrap();
        u.set_power_on(!u.get_power_on());
    });
    let weak = ui.as_weak();
    ui.on_set_mode(move |v| weak.unwrap().set_mode(v));
    let weak = ui.as_weak();
    ui.on_set_fan(move |v| weak.unwrap().set_fan(v));
    let weak = ui.as_weak();
    ui.on_set_wind(move |v| weak.unwrap().set_wind(v));
    let weak = ui.as_weak();
    ui.on_set_vpos(move |v| weak.unwrap().set_vpos(v));
    let weak = ui.as_weak();
    ui.on_set_hpos(move |v| weak.unwrap().set_hpos(v));
    let weak = ui.as_weak();
    ui.on_set_preset(move |v| weak.unwrap().set_target_temp(v));
    let weak = ui.as_weak();
    ui.on_set_pview(move |v| weak.unwrap().set_pview(v));
    let weak = ui.as_weak();
    ui.on_set_transport(move |v| weak.unwrap().set_transport_mode(v));
    let weak = ui.as_weak();
    ui.on_toggle_auto_refresh(move |v| weak.unwrap().set_auto_refresh(v));
    let weak = ui.as_weak();
    ui.on_set_close_behavior(move |v| weak.unwrap().set_close_behavior(v));
    let weak = ui.as_weak();
    ui.on_set_auto_start(move |enabled| weak.unwrap().set_auto_start(enabled));
    let weak = ui.as_weak();
    ui.on_set_toggle(move |key, v| {
        let u = weak.unwrap();
        match key.as_str() {
            "eco" => u.set_eco(v),
            "sleep" => u.set_sleep(v),
            "heater" => u.set_heater(v),
            "dryer" => u.set_dryer(v),
            "favoriteOn" => u.set_favorite(v),
            "windSensation" => u.set_wind_on(v),
            "verticalSwing" => u.set_vswing(v),
            "horizontalSwing" => u.set_hswing(v),
            "light" => u.set_light_on(v),
            "buzzer" => u.set_buzzer_on(v),
            _ => {}
        }
    });
    art_cache
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--interactive") {
        window_config::configure("software");
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select()?;
        let ui = MainWindow::new()?;
        fixture(&ui);
        let cycle_timer = slint::Timer::default();
        let minimize_cycle = args.iter().any(|a| a == "--minimize-cycle");
        if minimize_cycle || args.iter().any(|a| a == "--tray-cycle") {
            // Native software-renderer regression: hiding the last window must
            // not end the loop, and every restoration must repaint the scene.
            let weak = ui.as_weak();
            let mut step = 0;
            cycle_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(700), move || {
                let ui = weak.unwrap();
                step += 1;
                if step <= 10 {
                    if minimize_cycle {
                        ui.window().set_minimized(step % 2 == 1);
                    } else if step % 2 == 1 {
                        ui.hide().unwrap();
                        assert!(!ui.window().is_visible());
                    } else {
                        ui.show().unwrap();
                        ui.window().request_redraw();
                        assert!(ui.window().is_visible());
                    }
                }
            });
            ui.window().on_close_requested(|| {
                slint::quit_event_loop().unwrap();
                slint::CloseRequestResponse::KeepWindowShown
            });
            ui.show()?;
            slint::run_event_loop_until_quit()?;
        } else {
            ui.run()?;
        }
        return Ok(());
    }
    use slint::platform::{
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        Platform, WindowAdapter,
    };
    struct Headless(Rc<MinimalSoftwareWindow>);
    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone())))?;
    let ui = MainWindow::new()?;
    let art_cache = fixture(&ui);
    let export_clicks = Rc::new(std::cell::Cell::new(0));
    let count = export_clicks.clone();
    ui.on_export_log(move || count.set(count.get() + 1));
    ui.show()?;
    let output = PathBuf::from(args.first().map(String::as_str).unwrap_or("/tmp/miac-ui"));
    std::fs::create_dir_all(&output)?;
    window.set_size(slint::PhysicalSize::new(1440, 1100));
    for (theme, label) in [(0, "light"), (1, "dark"), (2, "macos"), (3, "senren"), (4, "senren-dark")] {
        ui.set_theme_index(theme);
        for view in 0..6 {
            ui.set_view(view);
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(1440, 1100);
            assert!(window.draw_if_needed(|renderer| {
                renderer.render(pixels.make_mut_slice(), 1440);
            }));
            let name = format!("{label}-{view}.png");
            let file = std::fs::File::create(output.join(&name))?;
            let mut encoder = png::Encoder::new(file, 1440, 1100);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()?
                .write_image_data(pixels.as_bytes())?;
            println!("{}", output.join(name).display());
        }
    }
    // Also render minimum-size windows to catch clipping under desktop resizing.
    window.set_size(slint::PhysicalSize::new(1160, 720));
    ui.set_theme_index(0);
    for view in [0, 1, 5, 4] {
        ui.set_view(view);
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(1160, 720);
        window.draw_if_needed(|renderer| {
            renderer.render(pixels.make_mut_slice(), 1160);
        });
        let file = std::fs::File::create(output.join(format!("compact-{view}.png")))?;
        let mut encoder = png::Encoder::new(file, 1160, 720);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()?
            .write_image_data(pixels.as_bytes())?;
    }

    // Keep the disconnected-device prompt fully visible at the shortest supported height.
    // This guards against layout containers stretching the card through the bottom edge.
    ui.set_view(0);
    ui.set_connected(false);
    ui.set_not_ready_reason("还没有设备信息，请先登录米家账号完成设备设置。".into());
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    let mut disconnected = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(1160, 720);
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(disconnected.make_mut_slice(), 1160);
    }));
    let file = std::fs::File::create(output.join("disconnected.png"))?;
    let mut encoder = png::Encoder::new(file, 1160, 720);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(disconnected.as_bytes())?;

    let panel_bottom = disconnected
        .as_bytes()
        .chunks_exact(3)
        .enumerate()
        .filter_map(|(index, rgb)| {
            let x = index % 1160;
            let y = index / 1160;
            (x >= 220 && rgb == [255, 255, 255]).then_some(y)
        })
        .max()
        .expect("disconnected prompt panel must be rendered");
    assert!(
        panel_bottom <= 696,
        "disconnected prompt must leave a 24px bottom margin, got bottom y={panel_bottom}"
    );
    ui.set_connected(true);
    ui.set_not_ready_reason("".into());
    window.set_size(slint::PhysicalSize::new(1440, 1100));

    // Exercise real pointer/key dispatch through the generated Slint tree.
    use slint::platform::{PointerEventButton, WindowEvent};
    let draw = || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(1440, 1100);
        window.draw_if_needed(|renderer| {
            renderer.render(pixels.make_mut_slice(), 1440);
        });
    };
    let click = |x: f32, y: f32| {
        let position = slint::LogicalPosition::new(x, y);
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    };
    ui.set_theme_index(0);
    ui.set_view(0);
    draw();
    click(801.0, 417.0);
    assert_eq!(
        ui.get_target_temp(),
        26.5,
        "plus button must adjust by half a degree"
    );
    click(407.0, 417.0);
    assert_eq!(
        ui.get_target_temp(),
        26.0,
        "minus button must adjust by half a degree"
    );
    for _ in 0..50 {
        click(801.0, 417.0);
        assert_eq!(ui.get_target_temp(), 26.5, "rapid plus click");
        click(407.0, 417.0);
        assert_eq!(ui.get_target_temp(), 26.0, "rapid minus click");
    }
    for y in [417.0, 390.0, 450.0] {
        window.dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(600.0, y),
        });
        assert_eq!(ui.get_target_temp(), 26.0, "hover must not adjust temperature");
    }
    for _ in 0..50 {
        let previous = ui.get_auto_refresh();
        click(1250.0, 47.0);
        assert_eq!(ui.get_auto_refresh(), !previous, "rapid auto-refresh click");
    }
    ui.set_target_temp(31.0);
    click(801.0, 417.0);
    assert_eq!(ui.get_target_temp(), 31.0, "upper bound");
    ui.set_target_temp(16.0);
    click(407.0, 417.0);
    assert_eq!(ui.get_target_temp(), 16.0, "lower bound");
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(600.0, 417.0),
        delta_x: 0.0,
        delta_y: 120.0,
    });
    assert_eq!(ui.get_target_temp(), 16.5, "dial scroll callback");
    // Hold past the page's flick gesture timeout so the full drag reaches the
    // dial instead of panning the surrounding page and moving later hit targets.
    for y in [450.0, 380.0] {
        window.dispatch_event(WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(600.0, 417.0),
            button: PointerEventButton::Left,
        });
        std::thread::sleep(std::time::Duration::from_millis(600));
        slint::platform::update_timers_and_animations();
        window.dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(600.0, y),
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(600.0, y),
            button: PointerEventButton::Left,
        });
        assert_eq!(ui.get_target_temp(), 16.5, "dragging must not adjust temperature");
    }
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(600.0, 417.0),
        delta_x: 0.0, delta_y: -120.0,
    });
    assert_eq!(ui.get_target_temp(), 16.0, "wheel down retains the model's temperature step");
    click(862.0, 232.0);
    assert!(!ui.get_power_on(), "power callback");
    click(850.0, 604.0);
    assert_eq!(ui.get_mode(), 5, "heat mode callback");
    click(700.0, 752.0);
    assert_eq!(ui.get_fan(), 5, "fan callback");
    click(1350.0, 451.0);
    assert!(!ui.get_eco(), "comfort toggle callback");
    for (view, y) in [(1, 193.0), (2, 243.0), (3, 293.0), (5, 343.0), (4, 393.0), (0, 143.0)] {
        click(100.0, y);
        assert_eq!(ui.get_view(), view, "sidebar navigation");
        draw();
    }
    ui.set_view(4);
    draw();
    click(820.0, 873.0);
    assert_eq!(export_clicks.get(), 1, "export log button callback");
    ui.set_view(3);
    draw();
    ui.set_raw_value("".into());
    click(450.0, 285.0);
    for ch in ["4", "2"] {
        window.dispatch_event(WindowEvent::KeyPressed { text: ch.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: ch.into() });
    }
    assert_eq!(
        ui.get_raw_value().as_str(),
        "42",
        "raw input must propagate to the Rust-visible window property"
    );
    // Render settings and the close prompt in both themes, including the
    // smallest supported window. No device or personal settings are touched.
    let capture = |name: &str, width: u32, height: u32| -> Result<slint::SharedPixelBuffer<slint::Rgb8Pixel>, Box<dyn std::error::Error>> {
        window.set_size(slint::PhysicalSize::new(width, height));
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
        assert!(window.draw_if_needed(|renderer| {
            renderer.render(pixels.make_mut_slice(), width as usize);
        }));
        let file = std::fs::File::create(output.join(name))?;
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(pixels.as_bytes())?;
        Ok(pixels)
    };
    // Exercise the actual nested log scroller: wheel and thumb dragging must
    // reach the final entry without scrolling the surrounding settings page.
    let original_log = ui.get_log_text();
    let log_lines: Vec<_> = (1..=200).map(|i| if i == 200 {
        "LOG 200 - END OF LOG".to_owned()
    } else { format!("LOG {i:03} - offline scroll check") }).collect();
    for (theme, pad, gap, bg) in [
        (0, 32.0, 20.0, [245, 247, 243]), (1, 32.0, 20.0, [23, 35, 30]),
        (2, 24.0, 16.0, [245, 245, 247]), (3, 28.0, 18.0, [250, 247, 251]),
        (4, 28.0, 18.0, [0, 0, 0]),
    ] {
        ui.set_view(0);
        ui.set_theme_index(theme);
        ui.set_log_text(log_lines.join("\n").into());
        ui.set_view(4);
        capture(&format!("logs-{theme}-layout.png"), 1440, 1280)?;
        // Theme changes animate the startup switch's colors for 140ms.
        // Settle those colors before comparing the stationary page pixels.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let top = capture(&format!("logs-{theme}-top.png"), 1440, 1280)?;
        let left = (220.0 + pad + 24.0) as usize;
        // Locate the fixed-height log well in the rendered page, using its
        // blank left padding rather than depending on header text height.
        let mut run = 0;
        let mut well_top = None;
        for y in 500..1270 {
            let index = (y * 1440 + left + 5) * 3;
            if top.as_bytes()[index..index + 3] == bg { run += 1; }
            else {
                if (145..=170).contains(&run) { well_top = Some(y - run - 1); break; }
                run = 0;
            }
        }
        let well_top = well_top.expect("settings must contain the complete log well");
        let well_width = ((1440.0 - 220.0 - 2.0 * pad - gap) * 0.62 - 48.0) as usize;
        let text_left = left + 10;
        let text_top = well_top + 10;
        let text_width = well_width - 20 - 18;
        let text_pixels = |image: &slint::SharedPixelBuffer<slint::Rgb8Pixel>| -> Vec<u8> {
            (text_top..text_top + 148).flat_map(|y| {
                let begin = (y * 1440 + text_left) * 3;
                image.as_bytes()[begin..begin + text_width * 3].iter().copied()
            }).collect()
        };
        let assert_page_still = |image: &slint::SharedPixelBuffer<slint::Rgb8Pixel>| {
            for (index, (before, after)) in top.as_bytes().chunks_exact(3)
                .zip(image.as_bytes().chunks_exact(3)).enumerate() {
                let (x, y) = (index % 1440, index / 1440);
                if x < left || x >= left + well_width || y < well_top || y >= well_top + 168 {
                    assert_eq!(before, after, "scrolling logs must not pan the settings page, theme {theme}, ({x},{y})");
                }
            }
        };
        let wheel = |delta_y: f32| window.dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new((text_left + 100) as f32, (text_top + 70) as f32),
            delta_x: 0.0, delta_y,
        });
        wheel(-100_000.0);
        let bottom = capture(&format!("logs-{theme}-wheel-bottom.png"), 1440, 1280)?;
        assert!(text_pixels(&top) != text_pixels(&bottom), "wheel must reveal later log entries");
        assert_page_still(&bottom);
        wheel(100_000.0);
        let back = capture(&format!("logs-{theme}-wheel-top.png"), 1440, 1280)?;
        assert!(text_pixels(&top) == text_pixels(&back), "wheel must return to the first log entry");
        let bar_x = (left + well_width - 17) as f32;
        let pressed = slint::LogicalPosition::new(bar_x, (text_top + 24) as f32);
        let released = slint::LogicalPosition::new(bar_x, (text_top + 136) as f32);
        window.dispatch_event(WindowEvent::PointerMoved { position: pressed });
        window.dispatch_event(WindowEvent::PointerPressed { position: pressed, button: PointerEventButton::Left });
        window.dispatch_event(WindowEvent::PointerMoved { position: released });
        window.dispatch_event(WindowEvent::PointerReleased { position: released, button: PointerEventButton::Left });
        let dragged = capture(&format!("logs-{theme}-drag-bottom.png"), 1440, 1280)?;
        assert!(text_pixels(&bottom) == text_pixels(&dragged), "dragging the scrollbar must reach the same final entry as the wheel");
        assert_page_still(&dragged);
        // A separate, shorter tail gives an independent reference for the
        // final visible entries, including the unique END OF LOG marker.
        ui.set_log_text(log_lines[180..].join("\n").into());
        capture(&format!("logs-{theme}-tail-layout.png"), 1440, 1280)?;
        wheel(-100_000.0);
        let tail = capture(&format!("logs-{theme}-tail-reference.png"), 1440, 1280)?;
        let actual = text_pixels(&bottom);
        let reference = text_pixels(&tail);
        // Shaping a shorter Text can move its baseline by one physical pixel.
        // Match the unique final entry's ink with a small vertical tolerance.
        let ink = |pixel: &[u8]| pixel.iter().zip(bg).any(|(a, b)| a.abs_diff(b) > 40);
        let mut best_match = 0.0_f64;
        for shift in -2_i32..=2 {
            let (mut actual_ink, mut reference_ink, mut shared_ink) = (0, 0, 0);
            for y in 132..148 {
                for x in 0..200.min(text_width) {
                    let expected = ink(&reference[(y * text_width + x) * 3..][..3]);
                    let moved_y = y as i32 + shift;
                    let present = (0..148).contains(&moved_y)
                        && ink(&actual[(moved_y as usize * text_width + x) * 3..][..3]);
                    actual_ink += usize::from(present);
                    reference_ink += usize::from(expected);
                    shared_ink += usize::from(present && expected);
                }
            }
            assert!(reference_ink > 20, "the reference must contain the final log entry");
            best_match = best_match.max(2.0 * shared_ink as f64 / (actual_ink + reference_ink) as f64);
        }
        assert!(best_match >= 0.98, "the final log entry must match the independent tail reference: {best_match:.3}");
        ui.set_log_text("".into());
        capture(&format!("logs-{theme}-cleared-layout.png"), 1440, 1280)?;
        let cleared = capture(&format!("logs-{theme}-cleared.png"), 1440, 1280)?;
        assert!(text_pixels(&cleared).chunks_exact(3).all(|pixel| pixel == bg), "clearing logs must remove stale text and reset the offset");
        ui.set_log_text("LOG RESET - visible after clearing".into());
        let reset = capture(&format!("logs-{theme}-reset.png"), 1440, 1280)?;
        assert!(text_pixels(&cleared) != text_pixels(&reset), "new logs must remain visible after clearing");
    }
    ui.set_log_text(original_log);
    ui.set_theme_index(0);
    println!("Runtime log scroll checks passed: 200 entries, wheel, draggable scrollbar, last-entry reference, clear/refill, no parent-page panning, all five themes");
    ui.set_view(0);
    ui.set_target_temp(24.5);
    ui.set_theme_index(0);
    capture("smooth-dial-light.png", 1440, 960)?;
    ui.set_theme_index(1);
    capture("smooth-dial-dark.png", 1160, 720)?;
    ui.set_theme_index(0);
    for temperature in [16.0, 26.0, 31.0] {
        ui.set_target_temp(temperature);
        capture(&format!("smooth-dial-{temperature}.png"), 1440, 960)?;
    }
    for scale in [1.0f32, 1.5, 2.0] {
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        for temperature in [16.0, 24.5, 26.0, 31.0] {
            ui.set_target_temp(temperature);
            let pixels = capture(&format!("aligned-marker-{temperature}-{scale}.png"),
                (1440.0 * scale) as u32, (960.0 * scale) as u32)?;
            assert_dial_marker_aligned(&pixels, scale as f64);
        }
    }
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });
    ui.set_target_temp(16.0);
    println!("Dial checks passed: drag disabled, wheel and buttons retained, marker aligned at four temperatures and 100/150/200% DPI");
    ui.set_view(0);
    ui.set_auto_refresh(false);
    capture("auto-refresh-off-light.png", 1440, 1100)?;
    ui.set_theme_index(1);
    capture("auto-refresh-off-dark.png", 1440, 1100)?;
    ui.set_auto_refresh(true);
    ui.set_theme_index(0);
    ui.set_view(4);
    ui.set_connected(false);
    ui.set_not_ready_reason("未连接设备".into());
    capture("settings-offline.png", 1440, 1100)?;
    // Startup settings are off by default and usable without connecting a device.
    assert!(!ui.get_auto_start(), "startup must be off by default");
    ui.set_auto_start_pending(true);
    click(550.0, 232.0);
    assert!(!ui.get_auto_start(), "pending startup writes cannot be clicked again");
    ui.set_auto_start_pending(false);
    click(550.0, 232.0);
    assert!(ui.get_auto_start(), "startup can be enabled");
    capture("settings-autostart-on.png", 1440, 1100)?;
    click(550.0, 232.0);
    assert!(!ui.get_auto_start(), "startup can be disabled");
    capture("settings-autostart-off.png", 1160, 720)?;
    // The theme entry sits immediately above software settings and works offline.
    ui.set_view(5);
    capture("software-theme-light.png", 1440, 1100)?;
    click(820.0, 300.0);
    assert_eq!(ui.get_theme_index(), 1, "dark theme belongs to its own page");
    capture("software-theme-dark.png", 1440, 1100)?;
    click(1210.0, 300.0);
    assert_eq!(ui.get_theme_index(), 2, "macOS can be selected while offline");
    capture("software-theme-macos.png", 1440, 1100)?;
    ui.set_view(0);
    capture("macos-offline-prompt.png", 1160, 720)?;
    ui.set_connected(true);
    for view in [5, 4, 0, 1] {
        ui.set_view(view);
        let pixels = capture(&format!("macos-compact-{view}.png"), 1160, 720)?;
        let background = (719 * 1160 + 1158) * 3;
        assert_eq!(&pixels.as_bytes()[background..background + 3], &[245, 245, 247],
            "macOS palette must reach page {view}");
    }
    ui.set_connected(false);
    ui.set_view(5);
    capture("software-theme-macos.png", 1440, 1100)?;
    click(450.0, 490.0);
    assert_eq!(ui.get_theme_index(), 3, "Senren theme can be selected while offline");
    capture("software-theme-senren.png", 1440, 1100)?;
    ui.set_connected(true);
    for view in [0, 1, 2, 3, 5, 4] {
        ui.set_view(view);
        capture(&format!("senren-{view}.png"), 1440, 960)?;
    }
    for view in [0, 1, 5, 4] {
        ui.set_view(view);
        capture(&format!("senren-compact-{view}.png"), 1160, 720)?;
    }
    // Select the new variant using the real second-row card while offline.
    ui.set_connected(false);
    ui.set_view(5);
    capture("software-theme-senren.png", 1440, 1100)?;
    click(820.0, 490.0);
    assert_eq!(ui.get_theme_index(), 4, "Senren dark can be selected while offline");
    capture("software-theme-senren-dark.png", 1440, 1100)?;
    ui.set_connected(true);
    for (width, height, label) in [(1440, 960, "full"), (1160, 720, "compact")] {
        for view in 0..6 {
            ui.set_view(view);
            let pixels = capture(&format!("senren-dark-{label}-{view}.png"), width, height)?;
            let corner = ((height - 2) * width + width - 2) as usize * 3;
            assert_eq!(&pixels.as_bytes()[corner..corner + 3], &[0, 0, 0],
                "pure black global background must reach page {view} at {width}px");
        }
    }
    ui.set_view(4);
    ui.set_cleanup_dialog_open(true);
    capture("senren-dark-cleanup.png", 1160, 720)?;
    ui.set_cleanup_dialog_open(false);
    ui.set_close_dialog_open(true);
    capture("senren-dark-close.png", 1160, 720)?;
    ui.set_close_dialog_open(false);
    // The software renderer must receive the exact physical size, not a fixed
    // thumbnail that it would shrink again using nearest-neighbour sampling.
    for theme in [3, 4] {
    ui.set_theme_index(theme);
    for scale in [1.0f32, 1.25, 1.5, 2.0] {
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        ui.set_view(0);
        capture(&format!("senren-{theme}-crisp-{scale}.png"), (1440.0 * scale) as u32, (960.0 * scale) as u32)?;
        let sizes = art_cache.borrow().cached_sizes();
        for logical in [26.0f32, 28.0, 36.0] {
            let physical = (logical * scale).round() as u32;
            assert!(sizes.contains(&(0, physical, physical)),
                "rendered {logical}px icons must request {physical}px textures at scale {scale}: {sizes:?}");
        }
        assert!(sizes.contains(&(6, (180.0 * scale) as u32, (240.0 * scale) as u32)),
            "background character must follow the actual screen scale");
        assert!(sizes.contains(&(7, (520.0 * 281.0 / 515.0 * scale).round() as u32, (520.0 * scale) as u32)),
            "right-side standing artwork must follow the actual screen scale: {sizes:?}");
        ui.set_view(5);
        capture(&format!("senren-{theme}-themes-crisp-{scale}.png"), (1160.0 * scale) as u32, (720.0 * scale) as u32)?;
        let sizes = art_cache.borrow().cached_sizes();
        assert!(sizes.contains(&(6, (32.0 * scale) as u32, (42.0 * scale).round() as u32)),
            "theme preview character must be rendered at its own physical size");
    }
    }
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });
    capture("senren-compact-5.png", 1160, 720)?;
    println!("Artwork checks passed: both Senren variants, native sizes at 100/125/150/200% DPI, right-side standing artwork, sidebar, cards and previews");
    for (view, y) in [(0, 112.0), (1, 156.0), (2, 200.0), (3, 244.0), (5, 288.0), (4, 332.0)] {
        click(90.0, y);
        assert_eq!(ui.get_view(), view, "Q版 navigation must retain its real page actions");
    }
    ui.set_connected(false);
    ui.set_view(5);
    capture("software-theme-senren.png", 1440, 1100)?;
    // A blue-themed window can return to either original theme immediately.
    click(820.0, 280.0);
    assert_eq!(ui.get_theme_index(), 1, "Senren can return to dark");
    capture("software-theme-dark.png", 1440, 1100)?;
    click(450.0, 300.0);
    assert_eq!(ui.get_theme_index(), 0, "light theme selection");
    capture("software-theme-compact.png", 1160, 720)?;
    println!("Theme UI checks passed: five choices, offline access, pure black across six pages, Q版 navigation, switching back, minimum window size");
    ui.set_view(4);
    capture("settings-offline.png", 1440, 1100)?;
    for (x, behavior) in [(766.0, 1), (1057.0, 2), (475.0, 0)] {
        click(x, 287.0);
        assert_eq!(ui.get_close_behavior(), behavior, "close preference selection");
    }
    assert_eq!(ui.get_close_behavior(), 0, "close preference can return to ask every time");
    // The delete button opens a modal even offline. Preview callbacks record
    // confirmation only; they never launch the helper or delete real data.
    capture("settings-cleanup-light.png", 1440, 1100)?;
    click(420.0, 370.0);
    assert!(ui.get_cleanup_dialog_open(), "delete button must open confirmation");
    let cleanup_confirmed = Rc::new(std::cell::Cell::new(0));
    let cleanup_cb = cleanup_confirmed.clone();
    let weak = ui.as_weak();
    ui.on_confirm_cleanup(move || {
        cleanup_cb.set(cleanup_cb.get() + 1);
        weak.unwrap().set_cleanup_dialog_open(false);
    });
    capture("cleanup-dialog-light.png", 1160, 720)?;
    click(576.0, 448.0);
    assert!(!ui.get_cleanup_dialog_open(), "cancel must dismiss the cleanup modal");
    assert_eq!(cleanup_confirmed.get(), 0, "cancel must not trigger deletion");
    ui.set_cleanup_dialog_open(true);
    ui.set_theme_index(1);
    capture("cleanup-dialog-dark.png", 1160, 720)?;
    click(692.0, 448.0);
    assert_eq!(cleanup_confirmed.get(), 1, "confirm must trigger cleanup exactly once");
    ui.set_theme_index(0);
    println!("Cleanup UI checks passed: settings button, offline access, cancel, confirm, light/dark compact dialogs");
    let confirmed = Rc::new(std::cell::Cell::new(None));
    let confirmed_cb = confirmed.clone();
    let weak = ui.as_weak();
    ui.on_confirm_close(move |to_tray, remember| {
        confirmed_cb.set(Some((to_tray, remember)));
        weak.unwrap().set_close_dialog_open(false);
    });
    ui.set_close_dialog_open(true);
    capture("close-dialog-light.png", 1440, 1100)?;
    ui.set_theme_index(2);
    capture("close-dialog-macos.png", 1160, 720)?;
    ui.set_theme_index(1);
    capture("close-dialog-dark-compact.png", 1160, 720)?;
    ui.set_theme_index(0);
    capture("close-dialog-light.png", 1440, 1100)?;
    click(520.0, 550.0);
    click(890.0, 636.0);
    assert_eq!(confirmed.get(), Some((true, true)), "remembered tray choice");
    ui.set_close_dialog_open(true);
    draw();
    click(740.0, 636.0);
    assert_eq!(confirmed.get(), Some((false, false)), "fresh prompt does not retain remember toggle");
    ui.set_close_dialog_open(true);
    draw();
    click(550.0, 636.0);
    assert!(!ui.get_close_dialog_open(), "cancel keeps the app open");
    ui.set_view(0);
    ui.set_connected(true);
    let notices = vec![
        ToastItem { kind: 1, text: "已下发 on".into(), seq: 0 },
        ToastItem { kind: 1, text: "目标温度已下发：26.5 ℃".into(), seq: 1 },
        ToastItem { kind: 2, text: "正在读取最新设备状态，请稍候".into(), seq: 2 },
    ];
    ui.set_toasts(Rc::new(VecModel::from(vec![notices[0].clone()])).into());
    capture("bottom-alignment-single.png", 1440, 960)?;
    ui.set_toasts(Rc::new(VecModel::from(notices)).into());
    capture("bottom-alignment-stacked.png", 1160, 720)?;
    println!("UI checks passed: temperature, auto-refresh, export-log, separate theme page, startup default/off/on/pending, close choices, navigation, raw input");
    ui.set_toasts(ModelRc::default());
    let p = miac_core::profile::Profile::parse("xiaomi.aircondition.ma7", &miac_core::profile::bundled_specs()["xiaomi.aircondition.ma7"])?;
    let [min,max,step] = p.temperature_range();
    ui.set_device_name("卧室空调 · 自动识别示例".into());
    ui.set_energy_unit("".into());
    ui.set_energy_hint("该型号未公开电量读数".into());
    ui.set_temp_min(min as f32);
    ui.set_temp_max(max as f32);
    ui.set_temp_step(step as f32);
    ui.set_target_temp(31.0);
    ui.set_extended_controls(false);
    let choices = |key: &str| ModelRc::new(VecModel::from(p.properties[key].choices.iter().map(|(v,n)| ControlChoice { value: *v as i32, label: n.clone().into() }).collect::<Vec<_>>()));
    ui.set_mode_options(choices("mode"));
    ui.set_fan_options(choices("fanLevel"));
    ui.set_can_vswing(p.writable("verticalSwing"));
    ui.set_can_hswing(p.writable("horizontalSwing"));
    ui.set_can_eco(p.writable("eco"));
    ui.set_can_sleep(p.writable("sleep"));
    ui.set_can_heater(p.writable("heater"));
    ui.set_can_dryer(p.writable("dryer"));
    capture("alternate-model.png", 1440, 1100)?;
    click(801.0, 417.0);
    assert_eq!(ui.get_target_temp(), 32.0, "alternate model uses 1 degree step and 32 degree limit");
    click(801.0, 417.0);
    assert_eq!(ui.get_target_temp(), 32.0);
    click(407.0, 417.0);
    assert_eq!(ui.get_target_temp(), 31.0);
    println!("Alternate-model UI checks passed");
    // Login-expiry prompt is cancellable without touching the local connection.
    ui.set_login_open(true);
    ui.set_login_state(0);
    ui.set_login_status("云端登录已过期，电量历史与温湿度读数需重新扫码登录。".into());
    ui.set_login_error("".into());
    ui.set_theme_index(0);
    capture("cloud-login-expired-light.png", 1160, 720)?;
    ui.set_theme_index(1);
    capture("cloud-login-expired-dark.png", 1160, 720)?;
    let started = Rc::new(std::cell::Cell::new(0));
    let started_cb = started.clone();
    ui.on_login_start(move || started_cb.set(started_cb.get() + 1));
    let weak = ui.as_weak();
    ui.on_login_cancel(move || weak.unwrap().set_login_open(false));
    click(580.0, 379.0);
    assert_eq!(started.get(), 1, "expired prompt can start QR login");
    click(710.0, 457.0);
    assert!(!ui.get_login_open(), "expired prompt can be dismissed");
    assert!(ui.get_connected(), "dismissing expiry prompt keeps local control");
    println!("Cloud-login-expiry UI checks passed");

    // Render a synthetic QR pattern without requesting or storing a login session.
    let mut qr = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(222, 222);
    for (index, pixel) in qr.make_mut_slice().iter_mut().enumerate() {
        let x = index % 222 / 6;
        let y = index / 222 / 6;
        let inside = (4..33).contains(&x) && (4..33).contains(&y);
        let finder = [(4, 4), (26, 4), (4, 26)].iter().find_map(|&(fx, fy)| {
            (x >= fx && x < fx + 7 && y >= fy && y < fy + 7).then(|| {
                let dx = x - fx;
                let dy = y - fy;
                dx == 0 || dx == 6 || dy == 0 || dy == 6
                    || ((2..5).contains(&dx) && (2..5).contains(&dy))
            })
        });
        let black = inside && finder.unwrap_or((x * 17 + y * 11 + x * y) % 5 < 2);
        *pixel = if black { slint::Rgb8Pixel::new(0, 0, 0) } else { slint::Rgb8Pixel::new(255, 255, 255) };
    }
    ui.set_login_open(true);
    ui.set_login_state(2);
    ui.set_login_qr(slint::Image::from_rgb8(qr));
    ui.set_login_seconds(293);
    ui.set_login_status("离线预览 · 示例二维码，不用于登录".into());
    for (width, height) in [(1440, 960), (1160, 720)] {
        for (theme, label) in [(0, "light"), (1, "dark"), (2, "macos"), (3, "senren"), (4, "senren-dark")] {
            ui.set_theme_index(theme);
            let pixels = capture(&format!("login-qr-{width}-{label}.png"), width, height)?;
            // Look within the white scan surface. Pure black theme backgrounds
            // must not be mistaken for the black QR pattern itself.
            let mut black_x = Vec::new();
            let scan_left = width as usize / 2 - 118;
            let scan_right = width as usize / 2 + 118;
            for row in pixels.as_bytes().chunks_exact(width as usize * 3) {
                let scan = &row[scan_left * 3..scan_right * 3];
                if scan.chunks_exact(3).filter(|rgb| *rgb == [255, 255, 255]).count() < 40 { continue; }
                black_x.extend(scan.chunks_exact(3).enumerate()
                    .filter_map(|(x, rgb)| (rgb == [0, 0, 0]).then_some((scan_left + x) as i32)));
            }
            let left = black_x.iter().min().expect("QR pattern must be rendered");
            let right = black_x.iter().max().unwrap();
            assert!((left + right - (width as i32 - 1)).abs() <= 2,
                "QR must be centered at width {width}: left={left}, right={right}");
        }
    }
    ui.set_theme_index(0);
    ui.set_login_state(3);
    ui.set_login_status("请选择空调，温湿度计会自动关联".into());
    ui.set_login_selected(0);
    ui.set_login_devices(Rc::new(VecModel::from(vec![
        LoginDevice {
            name: "空调 巨省电 大2匹新一级能效".into(), model: "xiaomi.airc.h53h00".into(),
            did: "preview-ac".into(), detail: "192.168.1.250 · 有token".into(), selectable: true,
        },
        LoginDevice {
            name: "米家智能温湿度计3".into(), model: "miaomiaoce.sensor_ht.t9".into(),
            did: "preview-thermo".into(), detail: "温湿度计 · 自动关联".into(), selectable: false,
        },
    ])).into());
    capture("login-devices-light.png", 1440, 960)?;
    let pixels = capture("login-devices-compact.png", 1160, 720)?;
    // Click the plain background of the thermometer row: it must not become selected.
    let picked = Rc::new(std::cell::Cell::new(None));
    let picked_cb = picked.clone();
    ui.on_login_pick(move |i| picked_cb.set(Some(i)));
    let row_y: Vec<_> = (0..720).filter(|&y| {
        let index = (y * 1160 + 730) * 3;
        pixels.as_bytes()[index..index + 3] == [240, 244, 236]
    }).collect();
    let y = (row_y.first().expect("thermometer row must be rendered") + row_y.last().unwrap()) / 2;
    click(730.0, y as f32);
    assert_eq!(picked.get(), None, "thermometer must not be selectable as an AC");
    let selected_y: Vec<_> = (0..720).filter(|&y| {
        let index = (y * 1160 + 730) * 3;
        pixels.as_bytes()[index..index + 3] == [226, 235, 219]
    }).collect();
    click(730.0, ((selected_y.first().unwrap() + selected_y.last().unwrap()) / 2) as f32);
    assert_eq!(picked.get(), Some(0), "air conditioner must remain selectable");
    ui.set_theme_index(1);
    capture("login-devices-dark.png", 1160, 720)?;
    ui.set_theme_index(0);
    let mut many_devices: Vec<_> = (0..9).map(|i| LoginDevice {
        name: format!("空调 {}", i + 1).into(), model: "xiaomi.airc.h53h00".into(),
        did: format!("preview-ac-{i}").into(), detail: "空调".into(), selectable: true,
    }).collect();
    many_devices.push(LoginDevice {
        name: "米家智能温湿度计3".into(), model: "miaomiaoce.sensor_ht.t9".into(),
        did: "preview-thermo".into(), detail: "温湿度计 · 自动关联".into(), selectable: false,
    });
    ui.set_login_devices(Rc::new(VecModel::from(many_devices)).into());
    let before = capture("login-devices-many.png", 1160, 720)?;
    let list_y: Vec<_> = (0..720).filter(|&y| {
        let index = (y * 1160 + 760) * 3;
        before.as_bytes()[index..index + 3] == [240, 244, 236]
    }).collect();
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(730.0, ((list_y.first().unwrap() + list_y.last().unwrap()) / 2) as f32),
        delta_x: 0.0, delta_y: -700.0,
    });
    let after = capture("login-devices-many-scrolled.png", 1160, 720)?;
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for y in 0..720 {
        let index = (y * 1160 + 760) * 3;
        if after.as_bytes()[index..index + 3] == [240, 244, 236] {
            if rows.last().and_then(|row| row.last()).is_none_or(|last| *last + 1 != y) {
                rows.push(Vec::new());
            }
            rows.last_mut().unwrap().push(y);
        }
    }
    let last_ac = &rows[rows.len() - 2];
    click(760.0, ((last_ac.first().unwrap() + last_ac.last().unwrap()) / 2) as f32);
    assert_eq!(picked.get(), Some(8), "ninth air conditioner must be reachable by scrolling");
    println!("Login UI checks passed: QR centered at both sizes/themes, sensor disabled, AC selection and scrolling past eight devices");
    ui.hide()?;
    Ok(())
}
