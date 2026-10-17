# 2026-10-17 最小化恢复闪屏修复

已修复恢复窗口时再次销毁、重建窗口造成的闪屏，并替换本机图形程序及三个现有安装包。保留原有主题、登录设置和此前工作区修改。

## 原因与改动

`window_config::install_minimize_recovery` 原先在最小化恢复后，通过零延时计时器执行 `hide()` → `show()`。由于软件渲染器配置了 `SLINT_DESTROY_WINDOW_ON_HIDE=1`，这会销毁已经恢复的 HWND，再创建新窗口。旧版原生回归的 10 次恢复均记录到一次销毁和 HWND 变化，5 次最大化恢复还丢失了最大化状态。

现在移除应用层恢复回调和 Slint 的 `native-minimized` 通知，由本地 winit 后端在 Windows 从最小化恢复时，标记整个客户区为脏区域并立即请求原生重绘。原窗口、大小、位置和最大化状态保持不变。单独记录 Windows 实际最小化状态，避免 Slint 先修改属性后漏掉恢复事件。普通设备状态更新仍使用局部绘制，真正隐藏到托盘时保留原有表面重建配置。

## 验证

| 项目 | 结果 |
| --- | --- |
| 旧版复现 | 10/10 次恢复销毁原窗口；原始截图、生命周期计数和报告均保留。 |
| 发布构建 | `cargo build -p miac-app --release --bin miac-app --examples --offline --locked` 通过。 |
| 应用测试 | `cargo test -p miac-app --bin miac-app --offline --locked`，62 项通过。 |
| 原生恢复 | 3 轮、30 次，覆盖五种主题、普通/最大化、原生/Slint 恢复路径和最小化期间更新状态。HWND、位置、大小和最大化状态全部保留，没有额外隐藏/销毁。 |
| 恢复初期画面 | 恢复后前 350ms 区间采集 589 帧，与完整参考帧比较，色阶差超过 12 的像素均为 0；未见旧版闪现桌面的画面。采样是原生客户区检查，不是连续屏幕录像。 |
| 原生移出屏幕重绘 | 3 轮 × 7 场景，21 项均为 0.000% 差异，覆盖五种主题、系统失效区、四个屏幕边缘和完全移出屏幕。 |
| 离线界面 | 全部回归通过，128 张截图中 126 张 PNG 字节相同；两张仅自启动开关动画区域有最大 1/255 色阶差，没有意外变化。 |
| 安装后真实窗口 | computer-use 检查普通窗口最小化恢复、最大化后最小化，再运行原启动程序唤起。主进程和 HWND 不变，画面完整，程序自身唤起保留最大化；最终留在普通窗口。 |

恢复回归命令：

```powershell
.\target\release\examples\window-repaint.exe target/window-repaint/restore --restore --rounds=3
```

本轮使用自动化工具的 `activate_window` 唤起时，观察到最大化窗口变为普通窗口。因此实际程序最大化验证使用第二次运行 `dist/miac-app.exe`，通过程序自己的单实例/托盘显示路径唤起；该路径已确认保留最大化。

## 本机替换与证据

图形 EXE SHA-256：`F80D90C57ADED3F56BAAEF2E673FF24306FDBB8873BCC8F1A3E089C492F641A1`。

- 已同步 `dist/miac-app.exe`、项目 `skills/miac-control/scripts/miac-app.exe` 和当前用户已安装 Skill 的图形 EXE。
- 已同步桌面 v2.2.0 ZIP、Skill v2.2.0 ZIP 和通用 Skill ZIP；只更新包内图形 EXE，其他条目内容哈希保持一致。助手程序未替换。
- 证据和备份：`D:\空调v2\target\minimize-restore-20261017-163406\`。`source-before/` 保存改动前源文件，`before-binaries/` 保存原 3 份 EXE 和 3 份 ZIP；两个 manifest 记录安装位置及替换前后哈希。
- `before/restore-results.json`、`after/restore-results.json`、`repaint-{1,2,3}.log`、`ui-comparison.json`、`package-checks.json` 和 `verification-summary.json` 记录各层验证。

本次为本机 Windows 原生窗口修复，没有执行真实空调写操作、数据清理或远程发布。
