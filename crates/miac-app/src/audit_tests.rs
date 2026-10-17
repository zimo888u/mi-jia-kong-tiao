//! Regressions found by the fresh source audit. All state and I/O are isolated.
use super::*;
use slint::Model;

#[test]
fn fresh_audit_raw_input_keeps_exact_integer_values() {
    assert_eq!(parse_raw_value("9007199254740993").unwrap(), serde_json::json!(9_007_199_254_740_993_u64));
    assert_eq!(parse_raw_value("18446744073709551615").unwrap().as_u64(), Some(u64::MAX));
    assert_eq!(parse_raw_value("-9223372036854775808").unwrap().as_i64(), Some(i64::MIN));
}

#[test]
fn fresh_audit_raw_input_trims_booleans_and_rejects_non_finite_numbers() {
    assert_eq!(parse_raw_value("  TRUE \n").unwrap(), serde_json::Value::Bool(true));
    assert_eq!(parse_raw_value(" false ").unwrap(), serde_json::Value::Bool(false));
    for input in ["NaN", "inf", "-inf", "1e999", "18446744073709551616"] {
        assert!(parse_raw_value(input).is_err(), "invalid raw input {input} must not become null or a rounded integer");
    }
    for input in ["+2", ".5", "1e-2"] { assert!(parse_raw_value(input).unwrap().is_number()); }
}

#[test]
fn fresh_audit_missing_clean_status_is_not_reported_as_stopped() {
    for value in [PropValue::Missing, PropValue::Err(-4001)] {
        assert_eq!(clean_status(true, &[("clean", value)]), "状态未知");
    }
    assert_eq!(clean_status(true, &[]), "状态未知");
    assert_eq!(clean_status(false, &[]), "该型号未提供");
    assert_eq!(clean_status(true, &[("clean", PropValue::Ok(true.into()))]), "运行中");
    assert_eq!(clean_status(true, &[("clean", PropValue::Ok(false.into()))]), "未运行");
    assert_eq!(clean_status(true, &[("clean", PropValue::Ok("1,3,20".into()))]), "运行中");
    assert_eq!(clean_status(true, &[("clean", PropValue::Ok("0,0,0".into()))]), "未运行");
    assert_eq!(clean_status(true, &[("clean", PropValue::Ok(1.into()))]), "运行中");
    assert_eq!(clean_status(true, &[("clean", PropValue::Ok(0.into()))]), "未运行");
    assert_eq!(clean_status(true, &[("clean", PropValue::Ok("invalid".into()))]), "状态未知");
}

#[test]
fn fresh_audit_warning_is_visible_without_a_background_refresh() {
    use slint::platform::{software_renderer::{MinimalSoftwareWindow, RepaintBufferType}, Platform, WindowAdapter};
    struct Headless(Rc<MinimalSoftwareWindow>);
    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> { Ok(self.0.clone()) }
    }
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = MainWindow::new().unwrap();
    // No Init command, credentials, migration, autostart access or device I/O.
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fresh-audit-tests/unused-ui-data");
    let creds = Credentials::isolated(&directory);
    let mut app = App {
        profile: miac_core::profile::Profile::default(), active_device_id: String::new(), switching_device_id: None,
        switching_since: None, pending_writes: pending_writes::PendingWrites::default(),
        last_snapshot_error: None, ui: ui.clone_strong(), settings: Settings::default(),
        settings_writer: settings_writer::SettingsWriter::spawn(creds.clone()),
        login: login_ui::LoginWorker::spawn(creds.clone()), creds, started: Instant::now(), log: vec![], toasts: vec![],
        props: vec![], connected: false, not_ready_reason: String::new(), link_label: String::new(), last_update: None,
        snapshot_in_flight: false, target_temp: TargetTempState::default(), diag: vec![], thermometer: None,
        maintenance: None, power: None, raw_result: None, selected_prop: 0, power_on_in_flight: None,
        login_open: false, cloud_login_required: false, login_state: 0, login_qr: None, login_status: String::new(),
        login_error: String::new(), login_seconds: 0, login_expires: None, login_devices: vec![], repaint_ticks: 0,
    };
    app.toast(2, "写入值不合法");
    assert_eq!(ui.get_toasts().row_count(), 1, "early-return errors must reach the UI immediately");
    app.toast(3, "写入值不合法");
    assert_eq!(ui.get_toasts().row_count(), 1);
    assert_eq!(ui.get_toasts().row_data(0).unwrap().kind, 3, "duplicate warnings must update their displayed severity");
    app.profile = miac_core::profile::Profile::legacy();
    app.maintenance = Some((vec![("clean", PropValue::Ok("1,0,0".into())), ("examine", PropValue::Ok(1.into())), ("runDuration", PropValue::Ok(200.into()))], true));
    app.refresh_view();
    assert_eq!(ui.get_clean_text().as_str(), "运行中");
    app.maintenance = None;
    app.refresh_view();
    assert_eq!(ui.get_clean_text().as_str(), "状态未知", "clearing maintenance must clear the old device's running state");
    assert_eq!(ui.get_examine_text().as_str(), "尚未读取");
    assert_eq!(ui.get_run_text().as_str(), "尚未读取");

    // Dispatch actual Slint events. Callback stand-ins never contact a device.
    use slint::platform::{PointerEventButton, WindowEvent};
    window.set_size(slint::PhysicalSize::new(1440, 1100));
    let draw = || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(1440, 1100);
        assert!(window.draw_if_needed(|renderer| { renderer.render(pixels.make_mut_slice(), 1440); }));
    };
    let click = |x, y| {
        let position = slint::LogicalPosition::new(x, y);
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    };
    let key = |text: slint::SharedString| {
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text });
    };
    let power_calls = Rc::new(std::cell::Cell::new(0));
    let count = power_calls.clone();
    ui.on_toggle_power(move || count.set(count.get() + 1));
    ui.set_connected(true);
    ui.set_power_known(true);
    ui.set_theme_index(0);
    ui.set_view(0);
    draw();
    click(862.0, 232.0);
    assert_eq!(power_calls.get(), 1, "baseline power callback must be reachable");
    let temperature = ui.get_target_temp();
    ui.set_login_open(true);
    ui.set_login_state(1);
    draw();
    click(862.0, 232.0);
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(600.0, 417.0), delta_x: 0.0, delta_y: 120.0,
    });
    assert_eq!(power_calls.get(), 1, "login backdrop must consume background power clicks");
    assert_eq!(ui.get_target_temp(), temperature, "login backdrop must consume dial wheel input");

    ui.set_login_open(false);
    ui.set_view(3);
    draw();
    ui.set_raw_value("".into());
    click(450.0, 285.0);
    key("4".into());
    assert_eq!(ui.get_raw_value().as_str(), "4", "baseline editor must receive keyboard focus");
    let writes = Rc::new(std::cell::Cell::new(0));
    let count = writes.clone();
    ui.on_raw_write(move || count.set(count.get() + 1));
    key(slint::platform::Key::Return.into());
    assert_eq!(writes.get(), 1, "baseline Enter must trigger the editor callback");
    let cancellations = Rc::new(std::cell::Cell::new(0));
    let count = cancellations.clone();
    let weak = ui.as_weak();
    ui.on_login_cancel(move || {
        count.set(count.get() + 1);
        weak.unwrap().set_login_open(false);
    });
    for state in 0..=4 {
        ui.set_login_open(true);
        ui.set_login_state(state);
        draw();
        key("5".into());
        key(slint::platform::Key::Return.into());
        key(slint::platform::Key::Tab.into());
        key(slint::platform::Key::Return.into());
        assert_eq!(writes.get(), 1, "login state {state} must block background Enter writes");
        assert_eq!(ui.get_raw_value().as_str(), "4", "modal keyboard focus must isolate text input");
        key(slint::platform::Key::Escape.into());
        assert!(!ui.get_login_open(), "Escape must cancel login state {state}");
    }
    assert_eq!(cancellations.get(), 5);
    // Real callback wiring must neither queue disconnected writes nor create a
    // MainWindow -> callback -> App -> MainWindow ownership cycle.
    let app = Rc::new(RefCell::new(app));
    let weak_app = Rc::downgrade(&app);
    let worker = Rc::new(Worker::spawn(Credentials::isolated(&directory), Settings::default()));
    App::wire_callbacks(&app, worker.clone());
    ui.invoke_set_mode(2);
    assert!(app.borrow().pending_writes.value("mode").is_none());
    {
        let mut a = app.borrow_mut();
        a.connected = true;
        a.profile = miac_core::profile::Profile::legacy();
        a.props = vec![("mode", PropValue::Ok(serde_json::json!(3)))];
    }
    ui.invoke_set_mode(2);
    assert_eq!(ui.get_mode(), 2, "a pending mode must survive refresh_view");
    {
        let mut a = app.borrow_mut();
        a.handle_events(vec![Event::Wrote { name:"mode".into(), ok:false, error:Some("synthetic rejection".into()) }], &worker);
        a.refresh_view();
        assert_eq!(ui.get_mode(), 3, "a rejected latest choice restores the observed mode");
        a.switching_device_id = Some("fake-switch".into());
        a.switching_since = Some(Instant::now() - Duration::from_secs(31));
        a.check_pending_deadlines();
        assert!(a.switching_device_id.is_none());
        assert!(!a.connected);
        a.connected = true;
        a.handle_events(vec![Event::Stopped { reason:"synthetic stopped worker".into() }], &worker);
        assert!(!a.connected);
        assert!(a.not_ready_reason.contains("stopped worker"));
    }
    drop(app);
    assert!(weak_app.upgrade().is_none(), "callback wiring must release App while the window still exists");
    worker.shutdown();
    assert!(!directory.exists(), "warning tests must never persist settings or credentials");
}
