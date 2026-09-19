//! Offline UI preview: no credentials, device connections, or settings writes.
//! cargo run -p miac-app --example ui-preview -- /tmp/miac-ui
//! Add --interactive to explore the five pages in a native window.
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{path::PathBuf, rc::Rc};
slint::include_modules!();

fn strings(values: &[&str]) -> ModelRc<SharedString> {
    Rc::new(VecModel::from(
        values.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
    ))
    .into()
}
fn fixture(ui: &MainWindow) {
    ui.set_device_name("米家空调 · 离线预览".into());
    ui.set_connected(true);
    ui.set_not_ready_reason("".into());
    ui.set_dark_theme(false);
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
        "室内温度",
        "室外温度",
        "压缩机频率",
        "室内盘管",
        "室外盘管",
        "排气温度",
        "运行功率",
        "电流",
        "累计运行",
    ]));
    ui.set_diag_values(strings(&[
        "29.0°C",
        "32.0°C",
        "42 Hz",
        "12.0°C",
        "38.0°C",
        "48.0°C",
        "620 W",
        "2.8 A",
        "128 小时",
    ]));
    ui.set_clean_text("待机".into());
    ui.set_examine_text("未运行".into());
    ui.set_run_text("128 小时".into());
    ui.set_fault_badge("正常".into());
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
    ui.set_migration_summary("离线预览：不读取或修改本机配置。".into());
    ui.set_log_text("14:32:06  设备状态更新成功\n14:32:00  已切换到局域网直连\n14:31:58  温湿度计已连接\n14:31:55  已载入本地设备配置".into());
    ui.set_about_text("mi / 米家空调 v2\n轻一点，也舒适一点。".into());
    let weak = ui.as_weak();
    ui.on_nav_to(move |v| weak.unwrap().set_view(v));
    let weak = ui.as_weak();
    ui.on_toggle_theme(move || {
        let u = weak.unwrap();
        u.set_dark_theme(!u.get_dark_theme());
    });
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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--interactive") {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select()?;
        let ui = MainWindow::new()?;
        fixture(&ui);
        ui.run()?;
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
    fixture(&ui);
    ui.show()?;
    let output = PathBuf::from(args.first().map(String::as_str).unwrap_or("/tmp/miac-ui"));
    std::fs::create_dir_all(&output)?;
    window.set_size(slint::PhysicalSize::new(1440, 1100));
    for dark in [false, true] {
        ui.set_dark_theme(dark);
        for view in 0..5 {
            ui.set_view(view);
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(1440, 1100);
            assert!(window.draw_if_needed(|renderer| {
                renderer.render(pixels.make_mut_slice(), 1440);
            }));
            let name = format!("{}-{view}.png", if dark { "dark" } else { "light" });
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
    ui.set_dark_theme(false);
    for view in [0, 1, 4] {
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
    ui.set_dark_theme(false);
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
    click(862.0, 232.0);
    assert!(!ui.get_power_on(), "power callback");
    click(850.0, 604.0);
    assert_eq!(ui.get_mode(), 5, "heat mode callback");
    click(700.0, 752.0);
    assert_eq!(ui.get_fan(), 5, "fan callback");
    click(1350.0, 451.0);
    assert!(!ui.get_eco(), "comfort toggle callback");
    for (view, y) in [(1, 193.0), (2, 243.0), (3, 293.0), (4, 343.0), (0, 143.0)] {
        click(100.0, y);
        assert_eq!(ui.get_view(), view, "sidebar navigation");
        draw();
    }
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
    println!("UI checks passed: temperature steps/bounds, scroll, power, mode, fan, comfort, navigation, raw input");
    ui.hide()?;
    Ok(())
}
