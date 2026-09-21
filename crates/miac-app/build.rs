// ─────────────────────────────────────────────────────────────────────────────
// build.rs —— 编译期把 .slint 编译成 Rust 代码
//
// 采用编译期编译（而不是运行时解释），好处是：
//   - 未用到的组件会被优化掉，二进制更小、常驻代码页更少
//   - 运行时不带 Slint 解释器，启动更快、内存更低
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn embed_windows_icon() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn find_rc() -> Option<PathBuf> {
        if let Some(sdk_dir) = std::env::var_os("WindowsSdkDir") {
            let bin = Path::new(&sdk_dir).join("bin");
            if let Ok(entries) = std::fs::read_dir(&bin) {
                let mut versions = entries.flatten().map(|entry| entry.path()).collect::<Vec<_>>();
                versions.sort();
                for version in versions.into_iter().rev() {
                    let rc = version.join("x64").join("rc.exe");
                    if rc.is_file() {
                        return Some(rc);
                    }
                }
            }
        }

        let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
        let mut versions = std::fs::read_dir(kits)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        versions.sort();
        versions
            .into_iter()
            .rev()
            .map(|version| version.join("x64").join("rc.exe"))
            .find(|rc| rc.is_file())
    }

    let rc = find_rc().expect("未找到 Windows SDK 的 rc.exe，无法嵌入应用图标");
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let assets = manifest_dir.join("assets");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("app-icon.res");
    let status = Command::new(rc)
        .current_dir(&assets)
        .args(["/nologo", "/fo"])
        .arg(&output)
        .arg("app-icon.rc")
        .status()
        .expect("运行 rc.exe 失败");
    assert!(status.success(), "编译 Windows 图标资源失败");

    println!("cargo:rustc-link-arg-bin=miac-app={}", output.display());
    println!("cargo:rerun-if-changed=assets/app-icon.png");
    println!("cargo:rerun-if-changed=assets/app-icon.ico");
    println!("cargo:rerun-if-changed=assets/app-icon.rc");
}

fn main() {
    #[cfg(windows)]
    embed_windows_icon();

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
