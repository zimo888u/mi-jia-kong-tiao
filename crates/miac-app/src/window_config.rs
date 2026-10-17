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
