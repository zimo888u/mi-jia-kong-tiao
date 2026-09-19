# 备份说明：v2 的 Slint 图形界面版（换成 TUI 之前的快照）

这个目录是 **2026-09-19 23:52** 对 `v2-rust\` 做的完整快照，
时间点是「**界面还是 Slint 图形界面**」的那一版。

## 为什么留它

`v2-rust\` 已被改造成终端界面（TUI）：`crates/miac-app` 里的 `.slint` 源码、
`build.rs`、系统托盘 `tray.rs` 都换成了 ratatui 渲染代码。那批文件在本目录里
**原样保留**，用途有三个：

1. **回退**：TUI 版唯一的功能倒退是没有系统托盘（终端没有通知区）。
   如果确实需要「关闭窗口收进托盘、常驻通知区」，把本目录的 `crates/miac-app`
   拷回去即可恢复图形界面版。
2. **对照**：两版共用同一个控制核心设计、同一套线程模型、同一份凭据与设置文件。
   改控制核心时可以用本版交叉验证行为有没有变。
3. **取证**：`docs-slint-ui\` 里是当时验收用的界面截图（含托盘恢复不白屏的取证图）。

## 内容

```
v2-rust-slint-backup-20260919-235251\
├─ Cargo.toml / Cargo.lock     当时的依赖（574 个 crate，TUI 版是 153 个）
├─ crates/
│  ├─ miac-core/               控制核心（与现在的 TUI 版基本一致，
│  │                           差别仅 controller.rs 的 UI_EXTRA_PROPS）
│  ├─ miac-app/                ★ Slint 界面：ui/*.slint + build.rs + src/tray.rs
│  └─ miac-cli/                命令行工具
├─ dist/                       当时的便携包（主程序 + 图标 + 启动脚本）
│                              ⚠ dist\miac-app.exe 是 11.7 MB 的 Slint 构建
├─ docs-slint-ui/              当时的界面截图（10 张 PNG）
└─ README.md                   当时的 README（标题为「Rust + Slint 原生版」）
```

## 怎么跑起来

本目录是独立工作区，直接构建即可（工具链见 `D:\DevTools\env.ps1`）：

```powershell
. D:\DevTools\env.ps1
cd v2-rust-slint-backup-20260919-235251
cargo build --release
.\target\release\miac-app.exe
```

注意：它用的是**同一份** `%APPDATA%\米家空调\` 凭据与 `settings.json`，
因此不需要重新扫码登录，两版可以来回切。

## 与 TUI 版的实测差异

| | 本目录（Slint） | `v2-rust\`（TUI） |
|---|---|---|
| 可执行文件 | 7.9 MB | 2.53 MB |
| 工作集（连真机 5 分钟） | 27.4 MB | 10.84 MB |
| 便携包 | 9.25 MB | 4.82 MB |
| 依赖 crate | 574 | 153 |
| 系统托盘 | ✅ | ❌ |
| 可跑在 SSH 里 | ❌ | ✅ |
