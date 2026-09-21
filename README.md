# 米家空调

一款面向 Windows 的米家空调桌面控制器。它用 Rust 和 Slint 构建，直接连接已授权的米家空调，让常用控制、电量数据、运行诊断和设备属性集中在一个轻量的原生客户端中。

![米家空调控制台预览](docs/ui/preview-light.png)

## 为什么使用它

- **本地优先**：优先通过局域网 miIO 连接设备；局域网不可用时可切换云端 RPC。
- **日常操作集中**：开关机、模式、目标温度、风速、摆风和舒适功能都在一个界面完成。
- **看得见设备状态**：提供电量统计、温湿度、运行诊断和高级属性读写。
- **适合长期运行**：单实例、系统托盘、自动刷新，以及关闭时“收进托盘 / 直接退出”的可记忆选择。
- **原生且轻量**：不依赖 Electron、Chromium 或 Node.js；发布版启用体积优化与 LTO。
- **安全保存凭据**：兼容旧版客户端配置，并支持 Windows DPAPI 加密保存。

## 下载与开始使用

从 [v2.1.0 发行版](https://github.com/zimo888u/mi-jia-kong-tiao/releases/tag/v2.1.0) 下载 Windows x86-64 便携版 `miac-app.exe`，解压后直接运行。

首次启动时，点击“扫码登录米家账号”，使用米家 App 扫码并选择要控制的空调。程序会将凭据保存到当前 Windows 用户的米家空调数据目录。

## 功能概览

| 页面 | 能做什么 |
| --- | --- |
| 控制台 | 调节开关、模式、温度、风速、摆风和舒适功能 |
| 电量统计 | 查看日、月度用电信息 |
| 运行诊断 | 查看连接状态和设备运行信息 |
| 高级属性 | 读取或写入设备公开属性 |
| 软件设置 | 切换主题、传输通道、自动刷新和关闭行为 |

## 从源码运行

需要 Rust stable 工具链和 Windows SDK（用于嵌入应用图标）。

```powershell
cargo run -p miac-app
```

启动登录弹层：

```powershell
cargo run -p miac-app -- --login
```

命令行诊断工具：

```powershell
cargo run -p miac-cli -- status
cargo run -p miac-cli -- creds
```

## 开发验证

```powershell
cargo test --workspace --locked
cargo run -p miac-app --example ui-preview -- docs/ui-preview
```

`ui-preview` 不连接设备、不读取凭据，会导出各页面的浅色与深色预览图并执行基础交互检查。

## 项目结构

```text
crates/miac-core/   miIO、云端 RPC、凭据、登录和设备控制核心
crates/miac-app/    Slint 桌面界面、Windows 托盘与应用图标
crates/miac-cli/    命令行诊断工具
docs/               UI 预览、验证资料与发行说明
```

## 兼容性与隐私

项目兼容旧版 Electron 客户端的 `device.json`、`cloud-session.json` 和 `thermometer.json`。它只访问用户选择并授权的米家设备；请勿将凭据、令牌或本地设备配置提交到仓库。

## 更新日志

完整记录见 [CHANGELOG.md](CHANGELOG.md)。
