/// Configure before selecting the backend or spawning any worker threads.
pub fn configure(renderer: &str) {
    // Slint 1.18's Windows software surface can retain its buffer age across
    // hide/show while Windows discards the displayed pixels. Dirty-only draws
    // then leave most of the restored window blank. Let winit release the
    // native window/surface on hide and recreate a fresh buffer on show.
    // Slint preserves the component tree, window position and size.
    #[cfg(windows)]
    if renderer == "software" {
        std::env::set_var("SLINT_DESTROY_WINDOW_ON_HIDE", "1");
    }
    #[cfg(not(windows))]
    let _ = renderer;
}

/// Native minimization does not go through Window::hide(). Recreate the software
/// surface once after restore, on the next event-loop turn (outside WM_SIZE).
pub fn install_minimize_recovery(ui: &crate::MainWindow, restored: impl Fn() + 'static) {
    #[cfg(windows)]
    {
        use slint::ComponentHandle;
        use std::{cell::Cell, rc::Rc, time::Duration};
        let weak = ui.as_weak();
        let was_minimized = Cell::new(false);
        let restored = Rc::new(restored);
        ui.on_native_minimized(move |minimized| {
            if !was_minimized.replace(minimized) || minimized {
                return;
            }
            let weak = weak.clone();
            let restored = restored.clone();
            slint::Timer::single_shot(Duration::ZERO, move || {
                let Some(ui) = weak.upgrade() else { return };
                let window = ui.window();
                // A later minimize or tray action supersedes this restoration.
                if !window.is_visible() || window.is_minimized() { return; }
                let maximized = window.is_maximized();
                if let Err(error) = window.hide().and_then(|_| window.show()) {
                    eprintln!("[窗口] 恢复绘制表面失败：{error}");
                    // Leave the application recoverable through the tray.
                    let _ = window.show();
                }
                window.set_maximized(maximized);
                window.request_redraw();
                restored();
            });
        });
    }
    #[cfg(not(windows))]
    let _ = (ui, restored);
}
