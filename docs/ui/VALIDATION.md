# 验证记录

日期：2026-09-20。环境：macOS / Apple Silicon，Rust 1.98.1，Slint 1.18.0，software renderer。

- `cargo test --workspace --locked`：65 项测试通过（62 项 core，3 项 app）。
- `cargo build -p miac-app --locked`：完整原生应用构建成功。
- `cargo run -p miac-app --example ui-preview -- /tmp/miac-ui`：五页浅/深主题共 10 张截图；1160×720 小窗口截图 3 张。已人工检查页面布局。
- UI 事件验证：温度加减、16/31°C 边界、滚轮、开关机、制热模式、风速、ECO、五页导航、原始属性文本输入均通过。
- 新安装及缺失主题字段默认浅色；显式保存的深/浅偏好均保留。
- `git diff --check`：通过。

已有非 Windows 警告：Windows DPAPI 常量 `ENTROPY` 在 macOS 未使用。未改动该平台专用逻辑。

限制：没有连接真实空调；Windows 托盘、凭据加密、打包及驻留内存指标未在此环境复测。界面快照采用独立预览中的示例数据，生产控制器仍使用实际数据。
