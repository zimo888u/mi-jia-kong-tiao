// ─────────────────────────────────────────────────────────────────────────────
// build.rs —— 编译期把 .slint 编译成 Rust 代码
//
// 采用编译期编译（而不是运行时解释），好处是：
//   - 未用到的组件会被优化掉，二进制更小、常驻代码页更少
//   - 运行时不带 Slint 解释器，启动更快、内存更低
// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    let config = slint_build::CompilerConfiguration::new()
        // 界面里的字号、间距都用 px 表达，这里不启用（也不禁用）缩放，
        // 保证在高 DPI 下由 winit 后端统一缩放，符合原版按像素布局的观感。
        .with_style("fluent".into());

    slint_build::compile_with_config("ui/main.slint", config)
        .expect("Slint 界面编译失败：请检查 ui/*.slint 的语法");

    println!("cargo:rerun-if-changed=ui/main.slint");
    println!("cargo:rerun-if-changed=ui/theme.slint");
    println!("cargo:rerun-if-changed=ui/common.slint");
    println!("cargo:rerun-if-changed=ui/dial-math.slint");
    println!("cargo:rerun-if-changed=ui/toast.slint");
    println!("cargo:rerun-if-changed=ui/pages/control.slint");
    println!("cargo:rerun-if-changed=ui/pages/power.slint");
    println!("cargo:rerun-if-changed=ui/pages/diag.slint");
    println!("cargo:rerun-if-changed=ui/pages/debug.slint");
    println!("cargo:rerun-if-changed=ui/pages/settings.slint");
}
