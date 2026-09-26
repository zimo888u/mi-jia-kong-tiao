---
name: miac-control
description: 通过对话控制当前 Windows 用户已授权的米家空调，查询开关、目标温度、模式、风速、摆风和当前室温。用户说“开空调”“调到 26 度”“现在室温多少”等时使用；适用于安装了本 Skill 的 Windows x64 电脑。
---

# 米家空调对话控制

使用本 Skill 目录下的 `scripts/miac-assistant.exe`。它与桌面端复用同一套 `miac-core` 和型号规格，从当前 Windows 用户的 `%APPDATA%\米家空调` 读取授权凭据，按真实型号校验每次写入。命令只向 stdout 输出一行 JSON；不要打开、复制或展示凭据文件。

优先以加载的 `SKILL.md` 所在目录定位可执行文件。默认 Codex 安装位置的 PowerShell 示例：

```powershell
$codexHome = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }
$miacExe = Join-Path $codexHome 'skills\miac-control\scripts\miac-assistant.exe'
& $miacExe room
```

## 命令

| 用户意图 | 参数 |
| --- | --- |
| 当前室温 | `room` |
| 指定空调传感器 / 米家温湿度计 | `room ac` / `room thermometer` |
| 空调完整状态 | `status` |
| 型号支持的温度范围、模式、风速、摆风 | `capabilities` |
| 开关机 | `power on` / `power off` |
| 设定目标温度 | `temp 26` |
| 模式 | `mode cool|heat|dry|fan|auto` |
| 风速 | `fan auto|max|<数字档位>` |
| 摆风 | `swing vertical|horizontal on|off` |

可在任一命令后附加 `--local` 或 `--cloud` 指定通道；默认沿用应用设置。先用 `capabilities` 确认型号支持的模式和档位，数字模式值不要自行猜测。`temp` 只接受该型号温度范围和步长上的值；空调关机时会先开机再设置。相对调温先读 `status` 的 `targetTemp`，不可用时请用户给出明确温度，不要以室温代替目标温度。

`room` 优先使用空调自带室温读数，缺失时尝试已配置的温湿度计；按 JSON 的 `source` 告知用户读数来源。不要把目标温度当作室温。只在读数存在时报告数值。

仅在用户明确要求对应操作时执行写命令。每次写入检查 `ok`、`acknowledged`、`verified` 和 `observed`：`verified=true` 才说已确认生效；收到确认但尚未读回时，说明已下发但未确认，并读一次 `status`。出现“写入结果未确认”或网络超时后，先读状态，勿盲目重发写命令。多项控制按用户要求逐项执行；某一步失败时停止后续写入并报告当前状态。

若 Codex 沙盒拒绝读取 `%APPDATA%` 或连接设备，可针对本次命令使用受控的工具权限提升。首次未登录时，使用本 Skill 附带的 `scripts/miac-app.exe --login` 打开桌面登录界面，让用户用米家 App 扫码并选择空调；不要通过对话索取 token 或云会话内容。安装与首次登录步骤见 [references/setup.md](references/setup.md)。

本 Skill 面向 Windows x64。只读验证可用 `help`、`room`、`status`；测试安装时不要无故改变空调状态。
