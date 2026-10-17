//! Native Windows repaint regression, with offline simulated device updates.
//! cargo run -p miac-app --example window-repaint -- target/window-repaint/check
//! Only this example's window is moved/invalidated; no credentials or device I/O.
use slint::ComponentHandle;
slint::include_modules!();
#[path = "../src/window_config.rs"]
mod window_config;
#[path = "../src/theme_art.rs"]
mod theme_art;

#[cfg(windows)]
mod native {
    use super::*;
    use std::{cell::{Cell, RefCell}, path::{Path, PathBuf}, rc::Rc, time::{Duration, Instant}};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC,
            DeleteObject, GetDC, GetDIBits, InvalidateRect, PatBlt, ReleaseDC, SelectObject,
            BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY, WHITENESS},
        Storage::Xps::{PrintWindow, PW_CLIENTONLY},
        System::Threading::GetCurrentProcessId,
        UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        UI::WindowsAndMessaging::{EnumWindows, GetClientRect, GetSystemMetrics, PW_RENDERFULLCONTENT,
            GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
            SetWindowPos, ShowWindow, SW_MINIMIZE, SW_RESTORE, WM_DESTROY, WM_SHOWWINDOW,
            SM_CXSCREEN, SM_CYSCREEN, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
            SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOSIZE, SWP_NOZORDER},
    };

    struct Frame { width: u32, height: u32, rgb: Vec<u8> }
    impl Frame {
        fn save(&self, path: &Path) -> Result<(), String> {
            let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
            let mut encoder = png::Encoder::new(file, self.width, self.height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().and_then(|mut w| w.write_image_data(&self.rgb))
                .map_err(|e| e.to_string())
        }
    }

    fn own_window() -> Result<HWND, String> {
        struct Search { pid: u32, hwnd: HWND }
        unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> i32 {
            let search = &mut *(data as *mut Search);
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == search.pid && IsWindowVisible(hwnd) != 0 {
                search.hwnd = hwnd;
                return 0;
            }
            1
        }
        let mut search = Search { pid: unsafe { GetCurrentProcessId() }, hwnd: std::ptr::null_mut() };
        unsafe { EnumWindows(Some(visit), &mut search as *mut _ as LPARAM); }
        if search.hwnd.is_null() { Err("offline preview window not found".into()) } else { Ok(search.hwnd) }
    }

    fn rect(hwnd: HWND) -> Result<RECT, String> {
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 { return Err("GetWindowRect failed".into()); }
        Ok(rect)
    }

    fn move_to(hwnd: HWND, x: i32, y: i32) -> Result<(), String> {
        if unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER) } == 0 {
            return Err("SetWindowPos failed".into());
        }
        Ok(())
    }

    fn capture_presented(hwnd: HWND) -> Result<Frame, String> {
        capture(hwnd, false)
    }

    /// Full-window reference independent of z-order and the retained surface.
    fn capture_reference(hwnd: HWND) -> Result<Frame, String> {
        capture(hwnd, true)
    }

    fn capture(hwnd: HWND, full_window_reference: bool) -> Result<Frame, String> {
        unsafe {
            let mut client: RECT = std::mem::zeroed();
            if GetClientRect(hwnd, &mut client) == 0 { return Err("GetClientRect failed".into()); }
            let (width, height) = (client.right - client.left, client.bottom - client.top);
            if width <= 0 || height <= 0 { return Err("window has an empty client area".into()); }
            let dc = GetDC(hwnd);
            let memory = CreateCompatibleDC(dc);
            let bitmap = CreateCompatibleBitmap(dc, width, height);
            if dc.is_null() || memory.is_null() || bitmap.is_null() {
                if !bitmap.is_null() { DeleteObject(bitmap); }
                if !memory.is_null() { DeleteDC(memory); }
                if !dc.is_null() { ReleaseDC(hwnd, dc); }
                return Err("cannot capture the native client surface".into());
            }
            let old = SelectObject(memory, bitmap);
            let copied = if full_window_reference {
                PrintWindow(hwnd, memory, PW_CLIENTONLY | PW_RENDERFULLCONTENT)
            } else {
                BitBlt(memory, 0, 0, width, height, dc, 0, 0, SRCCOPY)
            };
            SelectObject(memory, old);
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width, biHeight: -height, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB,
                ..std::mem::zeroed() };
            let mut bgra = vec![0u8; width as usize * height as usize * 4];
            let lines = GetDIBits(dc, bitmap, 0, height as u32, bgra.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);
            DeleteObject(bitmap); DeleteDC(memory); ReleaseDC(hwnd, dc);
            if copied == 0 || lines != height { return Err("native surface capture was incomplete".into()); }
            let rgb = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
            Ok(Frame { width: width as u32, height: height as u32, rgb })
        }
    }

    // Model Windows discarding visible pixels: changing a small UI field must
    // still repaint the entire OS-invalidated area, even though the DIB age is 1.
    fn invalidate_displayed_content(hwnd: HWND) -> Result<(), String> {
        unsafe {
            let mut client: RECT = std::mem::zeroed();
            if GetClientRect(hwnd, &mut client) == 0 { return Err("GetClientRect failed".into()); }
            let dc = GetDC(hwnd);
            if dc.is_null() { return Err("GetDC failed".into()); }
            let erased = PatBlt(dc, 0, 0, client.right, client.bottom, WHITENESS);
            ReleaseDC(hwnd, dc);
            if erased == 0 || InvalidateRect(hwnd, std::ptr::null(), 0) == 0 {
                return Err("could not invalidate the displayed client content".into());
            }
            Ok(())
        }
    }

    // Observe the original HWND, including transitions too short to catch in a
    // settled screenshot. A recovery must never hide or destroy this window.
    thread_local! {
        static RESTORE_TRACE: Cell<(u32, u32)> = const { Cell::new((0, 0)) };
    }

    unsafe extern "system" fn restore_trace(
        hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize,
    ) -> LRESULT {
        RESTORE_TRACE.with(|trace| {
            let (hidden, destroyed) = trace.get();
            trace.set((hidden + u32::from(msg == WM_SHOWWINDOW && wparam == 0),
                destroyed + u32::from(msg == WM_DESTROY)));
        });
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    fn different_percent(actual: &Frame, reference: &Frame) -> Result<f64, String> {
        if (actual.width, actual.height) != (reference.width, reference.height) {
            return Err("client dimensions changed after restoring".into());
        }
        let bad = actual.rgb.chunks_exact(3).zip(reference.rgb.chunks_exact(3))
            .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 12)).count();
        Ok(bad as f64 * 100.0 / (actual.width as usize * actual.height as usize) as f64)
    }

    fn run_restore_checks(ui: MainWindow, output: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
        let rounds: usize = std::env::args().find_map(|arg| arg.strip_prefix("--rounds=")
            .and_then(|n| n.parse().ok())).unwrap_or(3);
        if rounds == 0 { return Err("--rounds must be positive".into()); }
        let complete = Rc::new(Cell::new(false));
        let done = complete.clone();
        let failures = Rc::new(RefCell::new(Vec::<String>::new()));
        let errors = failures.clone();
        let report = Rc::new(RefCell::new(Vec::new()));
        let results = report.clone();
        let report_path = output.join("restore-results.json");
        let weak = ui.as_weak();
        let timer = slint::Timer::default();
        let mut phase = 0;
        let mut case = 0;
        let mut deadline = Instant::now();
        let mut restored_at = Instant::now();
        let mut original_hwnd: HWND = std::ptr::null_mut();
        let mut original_rect: RECT = unsafe { std::mem::zeroed() };
        let mut samples: Vec<(u128, Frame)> = Vec::new();
        let mut changed_handle = false;
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            if Instant::now() < deadline { return; }
            let Some(ui) = weak.upgrade() else { return };
            let theme = (case / 2 % 5) as i32;
            let maximized = case % 2 == 1;
            let native = (case / 10 + case % 2) % 2 == 0;
            let label = format!("round-{}-theme-{theme}-{}-{}", case / 10 + 1,
                if maximized { "maximized" } else { "normal" },
                if native { "native" } else { "slint" });
            let result = (|| -> Result<(), String> {
                match phase {
                    0 => {
                        ui.set_theme_index(theme);
                        ui.window().set_maximized(maximized);
                        deadline = Instant::now() + Duration::from_millis(500);
                        phase = 1;
                    }
                    1 => {
                        original_hwnd = own_window()?;
                        original_rect = rect(original_hwnd)?;
                        if unsafe { SetWindowSubclass(original_hwnd, Some(restore_trace), 1, 0) } == 0 {
                            return Err("could not observe native window lifecycle".into());
                        }
                        if native { unsafe { ShowWindow(original_hwnd, SW_MINIMIZE); } }
                        else { ui.window().set_minimized(true); }
                        deadline = Instant::now() + Duration::from_millis(250);
                        phase = 2;
                    }
                    2 => {
                        if unsafe { IsIconic(original_hwnd) } == 0 {
                            return Err(format!("{label}: the native window was not minimized"));
                        }
                        // Device updates while minimized must appear in the first restored frames.
                        ui.set_target_temp(23.5 + (case % 5) as f32 * 0.5);
                        ui.set_updated_at(format!("恢复检查 {case} · 离线模拟").into());
                        deadline = Instant::now() + Duration::from_millis(100);
                        phase = 3;
                    }
                    3 => {
                        samples.clear();
                        changed_handle = false;
                        RESTORE_TRACE.with(|trace| trace.set((0, 0)));
                        restored_at = Instant::now();
                        if native { unsafe { ShowWindow(original_hwnd, SW_RESTORE); } }
                        else { ui.window().set_minimized(false); }
                        phase = 4;
                    }
                    4 => {
                        let hwnd = own_window()?;
                        changed_handle |= hwnd != original_hwnd;
                        if unsafe { IsIconic(hwnd) } != 0 {
                            if restored_at.elapsed() > Duration::from_secs(2) {
                                return Err(format!("{label}: restore did not finish"));
                            }
                            return Ok(());
                        }
                        samples.push((restored_at.elapsed().as_millis(), capture_presented(hwnd)?));
                        if restored_at.elapsed() < Duration::from_millis(350) { return Ok(()); }
                        let reference = capture_reference(hwnd)?;
                        let (hidden, destroyed) = RESTORE_TRACE.with(Cell::get);
                        if unsafe { IsWindow(original_hwnd) } != 0 {
                            unsafe { RemoveWindowSubclass(original_hwnd, Some(restore_trace), 1); }
                        }
                        let new_rect = rect(hwnd)?;
                        let bounds_preserved = (original_rect.left, original_rect.top, original_rect.right, original_rect.bottom)
                            == (new_rect.left, new_rect.top, new_rect.right, new_rect.bottom);
                        let state_preserved = (unsafe { IsZoomed(hwnd) } != 0) == maximized;
                        let mut worst = 0.0_f64;
                        let mut frame_results = Vec::new();
                        for (index, (elapsed_ms, frame)) in samples.iter().enumerate() {
                            let diff = different_percent(frame, &reference)?;
                            worst = worst.max(diff);
                            frame_results.push(serde_json::json!({"elapsed_ms": elapsed_ms, "different_percent": diff}));
                            if index == 0 || index + 1 == samples.len() || diff > 1.0 {
                                frame.save(&output.join(format!("{label}-frame-{index:02}.png")))?;
                            }
                        }
                        reference.save(&output.join(format!("{label}-reference.png")))?;
                        let passed = !changed_handle && hidden == 0 && destroyed == 0
                            && bounds_preserved && state_preserved && worst <= 1.0;
                        println!("{label}: HWND preserved={}, hidden={hidden}, destroyed={destroyed}, bounds={bounds_preserved}, state={state_preserved}, {} early frames, worst {worst:.3}%", !changed_handle, samples.len());
                        results.borrow_mut().push(serde_json::json!({"case": label, "passed": passed,
                            "hwnd_preserved": !changed_handle, "hidden": hidden, "destroyed": destroyed,
                            "bounds_preserved": bounds_preserved, "maximized_preserved": state_preserved,
                            "worst_different_percent": worst, "frames": frame_results}));
                        if !passed { errors.borrow_mut().push(label); }
                        samples.clear();
                        case += 1;
                        if case == rounds * 10 { done.set(true); slint::quit_event_loop().unwrap(); }
                        phase = 0;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            })();
            if let Err(error) = result { errors.borrow_mut().push(error); slint::quit_event_loop().unwrap(); }
        });
        ui.show()?;
        slint::run_event_loop_until_quit()?;
        timer.stop();
        std::fs::write(report_path, serde_json::to_vec_pretty(&*report.borrow())?)?;
        if !complete.get() || !failures.borrow().is_empty() {
            return Err(format!("native restore regression: {:?}", failures.borrow()).into());
        }
        println!("Native restore checks passed: {} cycles, all five themes, normal/maximized, native/Slint restore, unchanged HWND and no extra hide/destroy", rounds * 10);
        Ok(())
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let output = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "target/window-repaint/check".into()));
        std::fs::create_dir_all(&output)?;
        window_config::configure("software");
        slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select()?;
        let ui = MainWindow::new()?;
        theme_art::install(&ui);
        ui.set_connected(true); ui.set_power_known(true); ui.set_power_on(true);
        ui.set_can_eco(true); ui.set_can_sleep(true); ui.set_extended_controls(true);
        ui.set_theme_index(4);
        ui.set_device_name("离线窗口重绘检查".into());
        let (screen_width, screen_height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        ui.window().set_size(slint::PhysicalSize::new((screen_width - 80).min(1440) as u32, (screen_height - 120).min(900) as u32));
        ui.window().set_position(slint::PhysicalPosition::new(24, 24));
        if std::env::args().any(|arg| arg == "--restore") {
            return run_restore_checks(ui, output);
        }
        let weak = ui.as_weak();
        let failures = Rc::new(RefCell::new(Vec::<String>::new()));
        let errors = failures.clone();
        let complete = Rc::new(Cell::new(false));
        let done = complete.clone();
        let timer = slint::Timer::default();
        let mut phase = 0;
        let mut case = 0;
        let mut original = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let mut returned: Option<Frame> = None;
        let cases = [(4, "plain-invalidation", 0), (4, "bottom-edge", 1), (4, "left-edge", 2),
            (3, "right-edge-light", 3), (0, "top-edge-light", 4), (2, "fully-offscreen-macos", 5),
            (1, "fully-offscreen-dark", 5)];
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(450), move || {
            let Some(ui) = weak.upgrade() else { return };
            let result = (|| -> Result<(), String> {
                let (theme, label, direction) = cases[case];
                let hwnd = own_window()?;
                match phase {
                    0 => { ui.set_theme_index(theme); original = rect(hwnd)?; }
                    1 => {
                        let (vx, vy, vw, vh) = unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN),
                            GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) };
                        let width = original.right - original.left;
                        let height = original.bottom - original.top;
                        let (x, y) = match direction {
                            1 => (original.left, vy + vh - 96),
                            2 => (vx - width + 96, original.top),
                            3 => (vx + vw - 96, original.top),
                            4 => (original.left, vy - height + 96),
                            5 => (vx + vw + 64, vy + vh + 64),
                            _ => (original.left, original.top),
                        };
                        move_to(hwnd, x, y)?;
                    }
                    2 => {
                        ui.set_target_temp(23.5 + case as f32 * 0.5);
                        ui.set_eco(case % 2 == 0); ui.set_sleep(case % 2 == 1);
                        ui.set_sensor_temp("25.1".into()); ui.set_sensor_hum("46".into());
                    }
                    3 => {
                        move_to(hwnd, original.left, original.top)?;
                        invalidate_displayed_content(hwnd)?;
                        ui.set_updated_at(format!("状态更新 {case} · 离线模拟").into());
                    }
                    4 => {
                        let frame = capture_presented(hwnd)?;
                        frame.save(&output.join(format!("returned-{label}.png")))?;
                        returned = Some(frame);
                        // The next tick obtains a full-window reference via
                        // WM_PRINT/PW_RENDERFULLCONTENT, independent of z-order
                        // and the retained presented surface under test.
                    }
                    5 => {
                        let reference = capture_reference(own_window()?)?;
                        reference.save(&output.join(format!("reference-{label}.png")))?;
                        let actual = returned.take().unwrap();
                        if (actual.width, actual.height) != (reference.width, reference.height) {
                            return Err(format!("{label}: client dimensions changed after restoring"));
                        }
                        let bad = actual.rgb.chunks_exact(3).zip(reference.rgb.chunks_exact(3))
                            .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 12)).count();
                        let total = actual.width as usize * actual.height as usize;
                        let bad_percent = bad as f64 * 100.0 / total as f64;
                        println!("{label}: {bad_percent:.3}% pixels differ from a fresh complete frame");
                        if bad_percent > 1.0 { errors.borrow_mut().push(format!("{label}: {bad_percent:.3}% missing/stale pixels")); }
                        case += 1;
                        if case == cases.len() { done.set(true); slint::quit_event_loop().unwrap(); }
                    }
                    _ => unreachable!(),
                }
                phase = (phase + 1) % 6;
                Ok(())
            })();
            if let Err(error) = result { errors.borrow_mut().push(error); slint::quit_event_loop().unwrap(); }
        });
        ui.show()?;
        slint::run_event_loop_until_quit()?;
        timer.stop();
        if !complete.get() { return Err(format!("native repaint check did not finish: {:?}", failures.borrow()).into()); }
        if !failures.borrow().is_empty() { return Err(format!("native repaint regression: {:?}", failures.borrow()).into()); }
        println!("Native repaint checks passed: seven invalidation/offscreen cases, simulated device updates, all five themes, no minimize needed for returned frames");
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    return native::run();
    #[cfg(not(windows))]
    Err("this regression checks the Windows native software surface".into())
}
