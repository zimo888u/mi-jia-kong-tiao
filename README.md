# 米家空调

Rust + Slint 原生桌面客户端，用于控制米家空调并查看电量、诊断、属性和温湿度数据。

## 下载

Windows x86-64 便携版：

[下载 miac-app.exe（v2.0.1）](https://github.com/zimo888u/mi-jia-kong-tiao/releases/download/v2.0.1/miac-app.exe)

下载后直接运行，无需安装 Node.js、Electron 或其他运行库。

首次启动后，点击“扫码登录米家账号”，使用米家 App 扫码并选择要控制的空调。凭据会保存到当前 Windows 用户的米家空调数据目录。

## 功能

- 局域网直连，云端 RPC 备用
- 扫码登录、设备选择和旧版凭据兼容
- 空调控制：开关机、模式、温度、风速、摆风和舒适功能
- 电量统计、运行诊断、高级属性读写和温湿度计
- 浅色/深色主题、系统托盘和自动刷新
- Windows DPAPI 凭据加密选项

## 从源码运行

需要 Rust stable 工具链。

```bash
cargo run -p miac-app
```

启动登录弹层：

```bash
cargo run -p miac-app -- --login
```

命令行诊断工具：

```bash
cargo run -p miac-cli -- status
cargo run -p miac-cli -- creds
```

## 开发验证

```bash
cargo test --workspace --locked
cargo run -p miac-app --example ui-preview -- /tmp/miac-ui
```

`ui-preview` 不连接设备、不读取凭据，会导出五个页面的浅色/深色截图并执行基础交互检查。

## 项目结构

```text
crates/miac-core/   控制核心、miIO/云端协议、凭据、登录和设备数据
crates/miac-app/    Slint 桌面界面和 Windows 托盘
crates/miac-cli/    命令行诊断工具
docs/               UI 预览图和验证资料
```

## 兼容性

设备凭据格式与旧版 Electron 客户端兼容，支持复用已有的 `device.json`、`cloud-session.json` 和 `thermometer.json`。程序默认优先使用局域网连接，失败时可切换云端通道。
