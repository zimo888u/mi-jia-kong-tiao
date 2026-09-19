# 米家空调 v2 —— Rust + Slint 原生版

单进程、无 Electron / Chromium / Node.js 的米家空调控制器。
界面用 Slint 编译期编译为原生组件，控制核心用 Rust 重写。

## Windows 下载

最新 Windows x86-64 便携版已经发布到 GitHub Releases：

[下载 miac-app.exe（v2.0.0）](https://github.com/zimo888u/mi-jia-kong-tiao/releases/download/v2.0.0/miac-app.exe)

下载后直接运行即可，无需安装 Node.js、Electron 或额外运行库。首次使用时，点击界面中的「扫码登录米家账号」，用米家 App 扫码并选择要控制的空调。

本版本包含 Sage 桌面 UI、未连接设备提示框布局修复，以及扫码登录成功事件和 `userId` 字符串格式兼容。

> 本文档记录**进度与实测数据**。总规划见仓库根目录的 `README.md`（v1 版）。

---

## 当前进度

| 阶段 | 内容 | 状态 |
|---|---|---|
| **1** | 内存原型：主窗口 + 五个页面 + 假数据 | ✅ **完成，验收通过** |
| **2** | 控制核心移植（miIO UDP / 云端 HTTPS / 加密 / 属性映射 / 自动刷新 / 重连） | ✅ **完成，已接界面并读通真机** |
| **3** | 扫码登录 + 设备选择 + 会话刷新 + DPAPI 凭据 | ✅ **完成，已真实扫码验证** |
| **4** | 界面逐页复刻（转盘 / 主题 / 提示 / 托盘） | ✅ **完成**（五页 + 明暗主题 + 右下角提示 + 系统托盘） |
| **5** | 首次启动自动迁移（凭据 / 主题 / 预设） | ✅ **完成** |
| **6** | 新旧并存回归测试后替换快捷方式 | ✅ **完成（旧版保留可回退）** |

### 第 3 阶段实测（2026-09-19 真实扫码，全流程跑通）

```
正在向小米申请二维码…
二维码已保存，已在系统图片查看器中打开
（等确认中…）
扫码确认成功，userId=<你的米家账号 ID>
已写入云会话：cloud-session.json
正在读取设备列表…
账号下共 7 个设备。
找到的空调：
  [1] 空调  model=xiaomi.airc.h53h00  did=<设备 did>  <局域网 IP>  token=有
已写入设备信息：device.json
已写入温湿度计：thermometer.json
```

扫码后新凭据立刻可用：局域网读取 **0.22 秒**，温湿度计重新绑定为
「米家智能温湿度计3」（25.8 ℃ / 42 % / 电量 100 %）。
凭据文件权限也已确认收紧到只有当前用户可访问（`icacls` 断继承生效）。

复现方式（三个入口）：

```powershell
# 图形界面：启动即打开登录弹层
.\target\release\miac-app.exe --login

# 命令行：出二维码并自动用图片查看器打开
.\target\release\miac-cli.exe login
```

第三个入口：设备未连接时，主界面遮罩上有「扫码登录米家账号」按钮。
以后要换账号也走这条路径。

**第 2 阶段已通真机**（2026-09-19 实测，设备型号 `xiaomi.airc.h53h00`）：

```
局域网直连  读 16 个属性耗时 0.30 秒     ← 默认通道
云端 RPC    读 16 个属性耗时 0.51 秒     ← 兜底通道
读到的真实值：on=false  targetTemp=27.5  roomTemp=29
             electricity=41.74 kWh  faultValue=4 (F2.4)
温湿度计：25.6 ℃ / 42 % / 电量 100 %
```

**托盘已完成并验收**（Win32 `Shell_NotifyIcon`，单进程）：

| 项目 | 实测 |
|---|---|
| 托盘图标 | 已注册到通知区（用 exe 自带图标，不依赖外部 .ico） |
| 左键点托盘 | 主窗口恢复并前置 |
| 右键菜单 | 显示主界面 / 开机·关机 / 退出 |
| 关闭按钮 | 收进托盘（进程不退出） |
| 托盘待机内存 | **28.7 MB**（目标 20–30 MB）✅ |
| 托盘期间进程数 | **1** ✅ |
| 恢复后画面 | 完整（PrintWindow 取证 153 色 vs 损坏时 12 色） |

复现命令：

```powershell
# 关闭 → 收托盘 → 待机内存
powershell -File tools\test-tray.ps1

# 关闭 → 托盘恢复 → 画面完好性
powershell -File tools\test-tray-menu.ps1
```

---


## 实测到的两个「非显然」协议细节

这两处都是**代码看不出问题、设备却完全不响应**，最后靠抓 v1 参考实现
（`node-mihome`）实际发出的字节才定位到。都补了回归测试防复发。

### 1. miIO 局域网握手不能用加密报文

miIO 第一步不是发加密的 `miIO.info`，而是发一个 **32 字节、几乎全是 `0xFF`
的空 hello**：

```
2131 0020 ffffffff ffffffff ffffffff ffffffffffffffffffffffffffffffff
魔术字 长度  未知段    设备ID    时间戳              校验和
```

关键在第 **4~7 字节也必须是 `0xFF`**。写成 0 时 `0x2131` 与长度 `0x20` 都对、
报文长度也一致，但设备直接丢弃，表现为 UDP 超时——极易误判成「设备离线」或
「网络不通」。设备从响应头里回传真实设备 ID 与时间戳，之后才用 token 派生的
密钥加密真正的 JSON 报文。

另外校验和是 `MD5(header ‖ token ‖ **密文**)`，漏掉密文同样会被丢包。

### 2. 小米云签名在请求体里，不是请求头

云端鉴权是 **form-urlencoded 请求体**里的 `data` / `_nonce` / `signature` 三个
字段，不是自定义请求头。早先按 `_s` 请求头实现，服务端直接回 401。要点：

- `nonce` = 12 字节：8 字节随机 + 4 字节「当前分钟数」大端
- `signedNonce` = `base64(SHA256(base64decode(ssecurity) ‖ nonce))`
- `signature` = `base64(HMAC-SHA256(key = signedNonce **原始字节**,
  msg = `path & signedNonce & nonce & data=…`))`
  （key 用 base64 字符串而不是原始字节会算出不同签名）
- Cookie 要带全 `sdkVersion / deviceId / userId / serviceToken / locale / channel`

---

## 第 1 阶段验收结果（本阶段的目标就是过这个门）

要求：**运行 5 分钟后总内存 < 45 MB**；若超标则改用纯 Win32/Direct2D。

### 实测数据

| 指标 | 要求 | 实测 | 结论 |
|---|---|---|---|
| 5 分钟驻留（工作集，假数据） | < 45 MB | **24.0 – 24.2 MB** | ✅ 通过 |
| 5 分钟驻留（工作集，接真机后） | < 45 MB | **27.4 – 27.8 MB**（实测跑到 7 分钟） | ✅ 通过 |
| 切页 + 刷新 100 次内存增长 | ≤ 5 MB | **+2.71 MB** | ✅ 通过 |
| 进程数 | 单进程或最多 2 个 | **1 个进程** | ✅ 通过 |
| 可执行文件体积 | 便携版 10–20 MB | **7.9 MB** | ✅ 优于目标 |

「接真机后」那行是最终数据：界面连着真空调、每 6 秒自动刷新、期间读到了
室温/电量/温湿度计，工作集稳定在 **27.4 MB**，私有提交 8.0–8.2 MB 全程不涨。

三条关键证据（都可复现）：

```powershell
cd v2-rust

# ① 5 分钟驻留内存：每 15 秒打印一次
.\target\release\miac-app.exe --probe

# ② 切页 100 次的内存增长（先预热 10 次取基线，避免把一次性建页成本算成泄漏）
.\target\release\miac-app.exe --self-test

# ③ 单元测试（53 项，含加密向量、云签名算法、miIO 握手字节、v1 凭据格式兼容）
cargo test --release

# ④ 不打开界面，直接用真实凭据读真机（排查通道/属性表用）
.\target\release\miac-cli.exe status          # 自动：局域网优先
.\target\release\miac-cli.exe status --cloud  # 强制云端
.\target\release\miac-cli.exe creds           # 只解析凭据，不联网
```

### 关于「+14 MB」的正确解读

冷启动水位约 13 MB，跑完第一轮五个页面后升到约 27 MB，
之后 **6 分钟不动**（27.4 MB → 27.8 MB）。

这 14 MB 是**一次性**的：Slint 的五个页面在首次切到时才构造元素树与布局缓存，
属于预热成本，不是泄漏。判据是它之后完全持平，且私有提交量稳定在 8.1 MB。

因此 `--self-test` 的基线取「预热一轮之后」的值，衡量的是真正的增长趋势；
绝对水位由 `--probe` 的长跑单独给出。

---

## 验收与回归（两条命令跑完）

```powershell
# 全量验收：进程数 / 冷启动 / 稳定内存 / 闲置 CPU / 托盘待机 / 体积 / 切页增长
powershell -File tools\acceptance.ps1

# 功能回归：把 v1 支持的操作在 v2 上真机跑一遍
.\target\release\regression.exe           # 只读
.\target\release\regression.exe --write   # 连同写操作（每项测完立即复位）
```

### 全量验收结果（2026-09-19 实测）

| 指标 | 要求 | 实测 | |
|---|---|---|---|
| 冷启动（到窗口出现） | < 2 秒 | **0.202 秒** | ✅ |
| 进程数 | 单进程或最多 2 | **1** | ✅ |
| 稳定内存（工作集） | 25–45 MB（硬上限 50） | **28.9 MB** | ✅ |
| 闲置 CPU | < 1 % | **0.00 %**（16 核，采样到增量 0） | ✅ |
| 托盘待机内存 | 20–30 MB | **28.9 MB** | ✅ |
| 收托盘后进程存活 | 不退出 | 仍在 | ✅ |
| 便携版体积 | 10–20 MB | **9.25 MB** | ✅ |
| 切页 + 刷新 100 次增长 | ≤ 5 MB | **+2.57 MB** | ✅ |

### 功能回归结果（局域网直连真机）

| 项目 | 结论 | 详情 |
|---|---|---|
| 状态快照（v1 `status`） | ✅ | 16 项全部读到 |
| 机器诊断（v1 `diag`） | ✅ | 8/8 项读到 |
| 维护状态 | ✅ | 自清洁 / 体检 / 累计运行 |
| 温湿度计 | ✅ | 25.8 ℃ / 42 % / 电量 100 % |
| 耗电统计（日/月/年） | ✅ | 本月 1.8 度、本年 41.6 度、18 天明细 |
| 原始属性读取（v1 `raw`） | ✅ | 2.1 = false |
| 属性表完整性 | ✅ | v1 的全部 40 项都能解析 |
| 参数校验 | ✅ | 温度/模式/风速/风感/定格 5 类规则与 v1 一致 |
| 指示灯写入 + 回读 + 复位 | ✅ | true → false → 回读一致 → 已复位 |
| 提示音写入 + 回读 + 复位 | ✅ | 同上 |
| ECO / 睡眠写入 | ⏭ 跳过 | 设备回 `err(-5000)`：**关机状态下不可写** |

最后那两条不是缺陷：v1 的 README 里就写着「属性返回 `err(code)`｜该属性当前
不可写（比如关机时不能调模式），先开机再设」。v2 的表现与 v1 完全一致，
所以回归脚本把它记为「设备按设计拒绝」而不是失败。
---

## 安装与回退（桌面快捷方式已指向 v2）

```powershell
# 装成默认（桌面「米家空调」→ v2；另建旧版回退快捷方式）
powershell -File tools\install-v2.ps1

# 回退到旧版 Electron
powershell -File tools\install-v2.ps1 -Revert
```

便携包在 `v2-rust\dist\`：**9.61 MB / 3 个文件**（主程序 + 图标 + 启动脚本），
无需安装、无需运行库，拷走就能用。

| 快捷方式 | 指向 |
|---|---|
| 米家空调 | `v2-rust\dist\miac-app.exe`（v2 原生版） |
| 米家空调（旧版 Electron） | `ac-ctl\dist\win-unpacked\米家空调.exe`（保留，可回退） |

> 关于进程名：v2 的可执行文件特意保持 `miac-app.exe`，**不叫「米家空调.exe」**。
> 否则任务管理器里新旧两版的进程名会完全一样，排查时根本分不清谁是谁。

旧版本体与源码目录**一个都没删**：`ac-ctl\` 整个保留，`dist\win-unpacked\`
也原样在。回退只是把快捷方式指回去。
---

## 目录结构

```
v2-rust/
├─ Cargo.toml                     工作区定义（release 开 LTO + 体积优化）
├─ .cargo/config.toml             构建配置与工具链说明
├─ crates/
│  ├─ miac-core/                  控制核心（无界面，可被命令行版/MCP 复用）
│  │  └─ src/
│  │     ├─ lib.rs                Transport 枚举等公共类型
│  │     ├─ miot.rs               ★ 属性表（由 tools/gen-miot.js 自动生成）
│  │     ├─ crypto.rs             MD5 / AES-128-CBC / SHA256 / HMAC / RC4
│  │     ├─ miio.rs               局域网 miIO（UDP 54321）协议实现
│  │     ├─ cloud.rs              云端 RPC（签名算法、统计、温湿度计）
│  │     ├─ controller.rs         通道选择与语义操作
│  │     ├─ worker.rs             工作线程：阻塞网络 + 断线重连 + 事件回传
│  │     ├─ credentials.rs        凭据定位/读写（兼容 v1 路径与 JSON 格式）
│  │     ├─ dpapi.rs              Windows DPAPI 加密（绑定用户+机器）
│  │     ├─ login.rs              小米账号扫码登录（协议，纯逻辑）
│  │     ├─ settings.rs           主题 / 预设温度 / 通道等偏好
│  │     ├─ migration.rs          首启自动迁移（含从 localStorage 扫主题）
│  │     ├─ tls.rs                rustls 配置（绕开损坏的 schannel）
│  │     └─ demo.rs               第 1 阶段用的假数据（现已仅用于属性名列表）
│  ├─ miac-app/                   图形界面（Slint）
│  │  ├─ build.rs                 编译期把 .slint 编译成 Rust
│  │  ├─ ui/
│  │  │  ├─ main.slint            主窗口：侧边栏 + 顶栏 + 五个页面 + 提示层
│  │  │  ├─ theme.slint           深/浅两套配色（与 style.css 逐值对应）
│  │  │  ├─ common.slint          卡片、分段按钮、开关、指标块等共用组件
│  │  │  ├─ toast.slint           右下角提示（最多 3 条，可点击关闭）
│  │  │  ├─ login.slint           扫码登录弹层（5 个状态）
│  │  │  ├─ dial-math.slint       表盘几何（含 Slint 单位陷阱的说明）
│  │  │  └─ pages/                control / power / diag / debug / settings
│  │  ├─ src/main.rs              界面状态、事件循环、回调接线
│  │  ├─ src/login_ui.rs          扫码登录的界面粘合层（登录线程 + PNG 解码）
│  │  ├─ src/tray.rs              系统托盘（Win32 Shell_NotifyIcon，单进程）
│  └─ miac-cli/                   命令行诊断工具（复用 miac-core）
│     └─ src/
│        ├─ main.rs               读状态/诊断/温湿度计/电量/原始属性
│        └─ bin/probe-miio.rs     miIO 报文探测器（对照参考实现排查协议）
└─ probe/                         独立小工程：用编译器当文档试探 Slint 语法
```

### 线程模型

```
  UI 线程 ──Command──▶ 工作线程（阻塞式 Controller：miIO UDP / 云端 HTTPS）
         ◀──Event────
```

网络调用一律不在 UI 线程上跑（miIO ~0.3 秒、云端 1~2 秒，直接调用会卡住重绘）。
两个 channel 解耦，UI 侧用非阻塞 `try_recv` 取事件；不引入 tokio，因为异步
运行时本身要占掉数 MB 常驻内存，而验收指标是 25–45 MB。

**工作线程按需重连**：连续失败 3 次且距上次重连超过 10 秒，就丢掉旧连接重新
`init_transport`；成功一次即清零。拔网线 / 路由器重启后无需手动刷新。

### 为什么有 `probe/`

Slint 的文档与实际 API 有出入（例如「Rectangle 能不能旋转」「Math.cos 吃角度还是弧度」
在文档里查不到确定答案）。`probe/` 是一个独立的小工作区，专门写最小用例让编译器回答，
比反复翻文档快得多。三个结论都来自它：

1. **Rectangle 在 Slint 1.18 没有任何旋转属性**（`transform-rotation-*`、`rotate-*` 都不存在）
   → 斜线改用 `Path`，表盘刻度改用三角函数直接算坐标。
2. **`Math.cos` / `Math.sin` 接受「角度」类型**，即「度数 × 1deg」，不是弧度。
   → 早期按弧度写导致表盘刻度半径塌缩、全部挤在一点。
3. **`@linear-gradient(180deg, a 0%, b 100%)` 语法正确且软件渲染后端支持**
   → 侧边栏渐变可用。

### 踩过的坑（都留了测试或注释，防止复发）

| 现象 | 真因 | 位置 |
|---|---|---|
| 局域网永远超时，像设备离线 | 握手 hello 的第 4~7 字节得是 `0xFF` 而不是 0 | `miio.rs` |
| 云端一直 401 | 签名要放请求体（form-urlencoded），不是 `_s` 请求头 | `cloud.rs` |
| 界面一直「连接中…」，日志无报错 | `main` 把 `app.borrow()` 持有了整个 `ui.run()` 期间 | `main.rs` |
| 定时器第一拍的事件被吞 | 事件取出后借不到 `App` 就丢了；改成放回 pending 队列 | `main.rs` |
| 后台线程静默死亡 | 工作线程 panic 没人打印；已加 `catch_unwind` 上报 | `worker.rs` |
| 侧边栏整块不显示 | Window 子元素只给 `width` 时高度塌成 0 | `main.slint` |
| 日历色块糊成一片 | 强调色太亮 + 透明度给太高（0.12~0.84），压到 0.10~0.55 | `power.slint` |
| 单元测试读到本机真实凭据 | `Credentials::new` 会挂回退目录；测试改用 `isolated` | `credentials.rs` |
| **托盘恢复后整屏纯白** | `SW_HIDE` 后再显示，Slint 软件渲染后端不重画 | `tray.rs` |

### 托盘那条最费劲：`SW_HIDE` 恢复后是白屏

「关闭按钮 → 收进托盘 → 点托盘恢复」是托盘的标准用法，但实测恢复后
**整屏纯白**。用 `PrintWindow` 取证很干净：正常时 153 种颜色，恢复后只剩 12 种
（等于内容没画）。下面这些补救**全部无效**：

- `window().request_redraw()`（连续多拍请求也没用）
- `InvalidateRect` + `UpdateWindow`
- `RedrawWindow`（无效化 + 擦除 + 子窗口 + 立即）
- `MoveWindow` 尺寸往返（**同一个调用从外部脚本发就有效，从进程内发无效**）
- 把窗口移到屏幕外 `-32000,-32000`（窗口仍算「可见」，但渲染同样停掉）

**最终方案：不用隐藏，改成把窗口压到所有窗口最底层**
（`SetWindowPos(HWND_BOTTOM)`）。窗口始终可见、始终参与合成，渲染表面一直有效；
实测遮挡 5 秒再拉回顶层，`PrintWindow` 仍是 153 色，画面完好。
使用体验上它就是「不见了」，与隐藏等效。

对比一下「压到底层」与「隐藏」的差别：

| | 托盘待机内存 | 恢复后画面 |
|---|---|---|
| `SW_HIDE` | 28.7 MB | ❌ 纯白（12 色） |
| 压到底层 | 28.7 MB | ✅ 完好（153 色） |

> 顺带记一笔：曾用 `SetProcessWorkingSetSize(-1,-1)` 把待机工作集修到 1.9 MB，
> 但**恢复后同样是白屏**——软件渲染的像素缓冲是堆上的 Vec，页面换出后再访问是
> 「重新调入并清零」。所以这条路也放弃了（28.7 MB 本来就达标）。

---

## 与 v1 的兼容性

### 属性表不靠手抄

`crates/miac-core/src/miot.rs` 是**自动生成**的，来源是 v1 的 `ac-ctl/lib/miot.js`：

```powershell
node tools/gen-miot.js
```

这样 v1 与 v2 的属性表逐项一致，不会出现「命令行对了、界面读错地址」这类跑偏。

### 凭据文件完全兼容

`credentials.rs` 的读写行为对齐 v1：

- 三个文件名与位置不变：`device.json` / `cloud-session.json` / `thermometer.json`
- 主目录仍是 `%APPDATA%\米家空调\`（与 v1 的 `app.getPath('userData')` 同一个目录）
- 读取时主目录优先、再回退旧目录，所以 v1 配好的凭据 v2 直接能用
- 字段名保持驼峰（`savedAt` / `localip` / `serviceToken` / `userId`）

单元测试里专门有 `session_json_roundtrip_is_v1_compatible` 与
`device_json_reads_v1_format` 两项守着这个契约。

### DPAPI 加密（v2 新增）

v1 只靠 icacls 文件权限保护明文 JSON。v2 增加 DPAPI：

- `CryptProtectData` 不传 `CRYPTPROTECT_LOCAL_MACHINE`，密钥由当前用户登录凭据派生
- 密文里混入机器密钥，**换用户或换电脑都解不开**
- 加了固定额外熵 `miac-app-v2-credentials`，别的程序即使调用 DPAPI 也解不开我们的密文

**并存策略**：写的时候默认仍写明文（新旧两版互相可读），
只有显式调用 `write_json_maybe_encrypted(.., encrypt = true)` 才写密文。
等第 6 阶段回归验证通过，再统一切到加密存储——这样测试期不会把自己锁在门外。

---

## 环境约束（换机器务必先读）

本机有两个坑，工具链和构建都绕不开，完整记录见 `D:\DevTools\README.md`：

1. **Windows schannel 凭据存储损坏**（`SEC_E_NO_CREDENTIALS`）
   → `curl` / `winget` / `Invoke-WebRequest` / VS 安装器的 .NET WebClient 全部连不上外网。
   → 只有 Node.js（自带 TLS 栈）和 cargo 能联网。
   → 云端 HTTPS 因此必须用 **rustls + webpki-roots**，不能依赖系统证书存储。

2. **官方源被限速到 ~10 KiB/s**
   → Rust 工具链走清华镜像（实测 15.4 MiB/s），crate 走清华稀疏索引。

3. **代理（127.0.0.1:7897，clash）时开时关，直连能力也跟着变**
   → 代理开着时直连不通、必须走代理；代理关掉后反而直连可以出网
     （2026-09-19 实测：代理进程消失后，Node 直连清华镜像返回 200）。
   → 所以**不要在任何地方写死代理**。cargo 那边的 proxy 配置已改成默认注释；
     程序侧统一走 `MIAC_PROXY` 环境变量，不设就是直连。
   → 踩坑记录：曾把代理写进 `D:\DevTools\Rust\cargo\config.toml` 与登录代码，
     代理一挂，cargo 编译和程序登录**双双卡死**。

---

## 下一步

1. **把控制核心接到界面**：控制器放到工作线程，用 channel 回传，替换 `demo.rs`
2. **托盘 + 最多三个右下角提示**：走 Win32 `Shell_NotifyIcon`，保持单进程
3. **扫码登录**：移植 `mi-qr-login.js` 的协议流程
4. **首启迁移**：读 v1 的凭据、预设温度与主题设置
5. **回归对比**：与 Electron 版逐项对照，通过后再替换桌面快捷方式

---

## 界面截图

`docs\` 下保留这些（都是实测截取，不是设计稿）：

| 文件 | 内容 |
|---|---|
| `v2-1-control.png` | 控制台页：温度刻度环、电源、模式、风速、摆风、风感、定格、舒适功能、预设 |
| `v2-2-power.png` | 电量统计页：今日/本月/本年 + 日历热力图（色块深浅 = 当日用电） |
| `v2-3-diag.png` | 运行诊断页：机器诊断九宫格、温湿度计、自清洁 / 维护、故障标志 |
| `v2-4-debug.png` | 高级 / 属性页：原始属性读写、全部状态属性、指示灯 / 提示音 / 效果 |
| `v2-5-settings.png` | 设置页：连接状态、通道切换、设备信息、凭据（DPAPI 入口）、迁移摘要、日志 |
| `v2-control-live.png` | 控制台页连着真机的样子（室温 29 ℃ / 累计 41.7 kWh） |
| `v2-installed.png` | 从桌面快捷方式启动后的样子 |
| `v2-login-modal.png` | 扫码登录弹层（`--login` 打开） |
| `v2-login-needed.png` | 未连接时的遮罩与「扫码登录米家账号」入口 |
| `v2-tray-restored.png` | 从托盘恢复后的界面（这张是证明「恢复不白屏」的取证） |

排查期间的临时截图（`tmp-*.png`、`probe-*.png`）已经清理掉了。
