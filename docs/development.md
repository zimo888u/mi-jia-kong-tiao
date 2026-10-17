# 开发与验证

需要 Rust stable 与 Windows SDK。桌面端、CLI 和 Skill 助手共用工作区，修改控制核心后应一起构建。

## 构建与测试

```powershell
cargo test --workspace --locked
cargo build --release --workspace --bins --examples --locked
```

Windows 托盘测试需要退出正在运行的米家空调实例，以免同名托盘窗口干扰。测试使用隔离凭据和模拟网络，不应清理当前用户数据。依赖已缓存时可追加 `--offline`。

## 离线界面

```powershell
cargo run -p miac-app --release --example ui-preview --locked -- target/ui-preview
```

覆盖五种主题、各功能页、二维码与设备筛选、不同型号、温度控制、登录过期，以及每种主题 200 条日志的滚轮与滑块操作。预览不读取用户凭据、不连接设备。输出只保存在被 Git 忽略的 `target/`。

## Windows 原生窗口

```powershell
cargo run -p miac-app --release --example window-repaint --locked -- target/repaint
cargo run -p miac-app --release --example window-repaint --locked -- target/restore --restore --rounds=3
```

前者检查窗口失效区域和移出屏幕后返回时的完整绘制；后者检查五种主题下普通与最大化窗口的最小化恢复，核对窗口句柄、隐藏/销毁事件、边界和早期帧。

这些检查使用自身窗口中的模拟状态，不代表所有型号都做过真机测试。真实设备检查应先选择只读命令；不得为验证打包而自动执行开关机、调温或数据清理。

## 分发

Skill 可运行 `skills/miac-control/package.ps1` 打包。发布包须同时包含 `LICENSE`、`LICENSE-NOTICE.md` 和 `THIRD-PARTY-NOTICES.md`，保留 Slint 署名及第三方声明。桌面端和助手应使用同一版本。

AFPL 发行版同时提供完整对应源码包，包含项目、依赖源码及可离线解析的 Cargo 构建配置；第三方代码保留原有许可。完整源码包与程序包放在同一发行页，参见 `LICENSE` 第 2(c) 节。

发版前检查源码标签、包内文件、SHA-256 和公开下载结果。运行日志、凭据、审计原稿、机器路径与个人验收截图仅在本地保存，不应加入源码或发行包。
