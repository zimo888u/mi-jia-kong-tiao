# 2026-10-15 发布版验证与现有程序替换

> 最新图形程序替换与恢复窗口验证见 [2026-10-17 最小化恢复闪屏修复](minimize-restore-2026-10-17.md)。

> 本文记录首次安全修复版本的验证。随后已按用户要求继续降低内存，并替换为新构建；当前内存数据、产物哈希与验证结果见 [内存优化记录](memory-optimization-2026-10-15.md)。

已按用户“验证并替换现有”的要求，在 D 盘构建并替换现有版本。桌面端、项目内 Skill、当前用户已安装 Skill 使用同一批发布产物。保留现有登录和主题设置，未执行真实设备写命令或数据清理。

## 验证结果

| 项目 | 结果与证据 |
| --- | --- |
| 工作区回归 | 上一阶段 `cargo test --workspace --offline --locked` 共 211 项通过；本次未把它重复计为新增测试。 |
| 发布编译 | `cargo build --release --workspace --bins --examples --offline --locked` 通过。首次发现界面预览调用的 `ArtCache::cached_sizes` 仅在 debug 中提供，已移除该方法的 debug 限制后重新编译通过。 |
| 发布版 CLI | 10 项实际进程检查通过，包括错误参数退出码、无凭据失败、通道就绪后读失败、代理日志脱敏和拒绝旧清理参数；只用合成凭据和本地假代理。 |
| 发布版界面回归 | `ui-preview.exe` 全部通过：温度与主题交互、各页、二维码居中、设备筛选、离线状态、关闭与清理弹层、登录过期、不同型号、DPI 图片尺寸检查。均为离线测试。 |
| 发布版原生重绘 | 连续 3 轮，每轮 7 个场景，全部为 `0.000%` 像素差异；覆盖五种主题、普通失效重绘和移出屏幕再返回。未复现历史间歇性问题，未声称查明其旧失败根因。 |
| 构建产物真机读取 | 发布版助手分别以自动、强制局域网、强制云端读取，3 项成功。 |
| 安装后真机读取 | 从当前用户已安装 Skill 目录实际运行助手，再次分别以自动、强制局域网、强制云端读取，3 项成功。 |
| 真实窗口 | 用发布产物打开主窗口，控制页和运行诊断页显示实时数据，点击“退出程序”后进程退出。替换后通过 `dist/启动空调.cmd` 实际打开窗口。 |
| 现有桌面入口 | 执行桌面“米家空调.lnk”，复用同一新版实例；最终唯一主进程 PID 11224，路径为 `D:\空调v2\dist\miac-app.exe`。 |
| 二进制和包 | 5 份 EXE、3 份 ZIP 均已替换，SHA-256 与发布/暂存产物一致；ZIP 内 EXE 也独立解包流校验。 |

真机为 `xiaomi.airc.h53h00`。两通道读到的关键状态一致：关机、设定温度 24.5℃、空调室温 25℃，属性错误集合为空。设备仍返回故障属性值 4，界面按既有规则展示 F2.4，并注明可能为当前或锁存/历史上报；本次未复位故障。温湿度计没有有效读数，界面保留不可用状态，没有以目标温度代替室温。

## 替换范围

| 位置 | 现有文件 |
| --- | --- |
| 桌面端 | `D:\空调v2\dist\miac-app.exe` |
| 项目 Skill | `D:\空调v2\skills\miac-control\scripts\miac-app.exe`、`miac-assistant.exe` |
| 当前用户已安装 Skill | `C:\Users\SilentZZm\.codex\skills\miac-control\scripts\miac-app.exe`、`miac-assistant.exe` |
| 桌面安装包 | `D:\空调v2\dist\miac-app-v2.2.0-windows-x86_64.zip` |
| Skill 安装包 | `D:\空调v2\dist\miac-control-skill-v2.2.0-windows-x64.zip`、`miac-control-skill-windows-x64.zip` |

现有桌面快捷方式继续指向原位置。Skill 的说明文件与项目一致，无须覆盖。历史 `v2.1.0` 包保留。此处只更新本机产物，未上传或修改 GitHub 发行版。

| 发布文件 | SHA-256 |
| --- | --- |
| miac-app.exe | `9BBF869AC33ABEB9E835DEAB2B62509D6FD3F51E3A16459EF3BE93E0C6B4531D` |
| miac-assistant.exe | `2CABA510258E3DF1436FE475B9091E3A9830D083548463106C334CCE5A3733D2` |
| miac-cli.exe（构建目录） | `7781163C5A37326CFDC5E17A7EC320B1AC1B88B4CF1B86E6DDD6732FB138FE2A` |

## 备份与证据

- 目录：`D:\空调v2\target\deployment-20261015-132401\`。
- `before/` 保存替换前的 5 份 EXE、启动脚本和 3 份 ZIP，共 9 份文件。只备份程序文件，未复制用户凭据。
- `backup-manifest.json` 记录每个原位置、备份位置及原 SHA-256；`installed-manifest.json` 记录替换后的 SHA-256。
- `release-build.log`、`ui-release.log`、`ui-release/`、`repaint-release-{1,2,3}.log` 及同名截图目录保存编译和 UI 回归证据。
- `live-status-release.json`、`live-status-installed.json` 保存脱敏后的真机读取结果。
- 发布版 CLI 记录位于 `target/audit-remediation-20261015/cli-validation-release.json`。
- 首次替换脚本因 PowerShell 将空备份参数转换为空字符串而失败，当时 9 个原文件哈希均未改变。改用 `NullString.Value`，在独立临时文本上验证替换 API 后成功执行；替换逐个使用同目录暂存和原子替换。

## 验证边界

- 未重新扫码登录账号、未切换真实账号/设备，二维码和切换边界由离线回归覆盖。当前已有授权可继续使用。
- 未开关机、调温、改变模式/风速/摆风或执行真实数据清理，不能把只读真机验证表述为实际写控制全量验收。
- 主窗口运行时观察到工作集约 59.96 MiB、私有内存约 31.46 MiB；本次未做五分钟驻留测量，不能宣称达到旧代码注释中的 50 MB 工作集目标。
- Clippy 仍受本机缺少组件、下载源 404 限制；未将现有在线 OSV 查询称为 cargo audit。
