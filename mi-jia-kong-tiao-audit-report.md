# mi-jia-kong-tiao 代码审计报告

- 审计对象：`https://github.com/zimo888u/mi-jia-kong-tiao`（Rust + Slint，Windows 米家空调控制器），`main` 分支快照
- 审计范围：110 个文件中全部自研源码 —— `crates/miac-core`(14 个 .rs)、`crates/miac-app`(9 个 .rs + 13 个 .slint)、`crates/miac-cli`、`skills/miac-control`、构建脚本与 `.cmd`
- 审计方式：全量静态阅读 + 交叉调用点可达性分析；对可实证的部分做了本机实验（Win32 消息链、icacls 权限语法、日期算法、规格表数据穷举）
- **未做**：编译、`cargo clippy`、`cargo test`（本机无 Rust 工具链）。因此本报告不包含编译期问题，也不排除"能编译但在运行时才暴露"的问题
- 只读保证：全程未修改、未创建、未删除仓库内任何文件；审计副本位于临时目录

---

## 一、结论摘要

| 级别 | 条数 | 说明 |
| --- | --- | --- |
| 高 | 2 | 互相串联：一个线程级崩溃 + 一个"崩溃后无人知道"的静默失效 |
| 中 | 9 | 功能错误、特定条件下完全失效、UI 可感知缺陷、回归测试体系失效 |
| 低 | 38 | 健壮性、显示、资源、工具链、工程卫生 |

代码整体质量明显高于同类个人项目：注释记录了真实的协议坑（miIO hello 包、校验和覆盖范围、Slint 借用重入），非测试路径几乎没有裸 `unwrap`，凭据链路（DPAPI + 真实 SID 的 ACL + 原子写）实现扎实。**缺陷集中在"输入信任边界"、"状态机残留"和"错误不可见"三类。**

---

## 二、高危（建议优先修）

### H1. 工作线程崩溃后界面完全无感知，控制永久静默失效

**位置**：`crates/miac-core/src/worker.rs:159-164`、`worker.rs:154-156`、`crates/miac-app/src/main.rs:1799`

```rust
// worker.rs:159 —— Disconnected 与 Empty 被归一成同一个 None
pub fn try_recv(&self) -> Option<Event> {
    match self.rx.try_recv() {
        Ok(e) => Some(e),
        Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
    }
}
// worker.rs:154 —— 发送失败被吞
pub fn send(&self, cmd: Command) { let _ = self.tx.send(cmd); }
```
```rust
// main.rs:1799 —— UI 每 200ms 取一次事件
let mut events = std::mem::take(&mut pending);
tick_worker.drain(&mut events);
```

**问题**：worker 线程一旦结束（内部 panic 被 `catch_unwind` 接住后只补发一条 `Event::Log` 文本，随后线程结束、`evt_tx` 被 drop），事件接收端进入 `Disconnected` 状态，但 UI 侧把它当成"当前没有事件"。`connected` 保持 `true`、状态栏继续显示"已连接"，而此后所有 `Command::*` 全部静默丢弃。

**后果**：界面永久停跳，托盘开关机、调温、刷新全部无效，只有日志区一行"[工作线程] 已崩溃"（普通日志，不是状态事件），用户只能重启进程。这条把 `catch_unwind` 兜底的工程价值削掉了一半。

**修法方向**：把 `Disconnected` 与 `Empty` 区分开，或给 `Worker` 暴露 `is_alive()`；UI 检测到后切到明确的错误态并提示"后台通道已停止，请重启"。

---

### H2. `parse_token` 对非 ASCII 的 32 字节 token 必然 panic

**位置**：`crates/miac-core/src/miio.rs:454-469`

```rust
pub fn parse_token(hex_str: &str) -> Result<Vec<u8>, MiioError> {
    let s = hex_str.trim();
    if s.len() != 32 { return Err(...); }          // ← 字节长度，不是字符数
    let mut out = Vec::with_capacity(16);
    for i in (0..32).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)   // ← 按字节下标切片
            .map_err(|_| MiioError::Protocol(...))?;
```

**触发**：`"a"×29 + "€"` 这类"32 字节但不是 32 个 ASCII 字符"的 token。`i=28` 时 `&s[28..30]` 的结束位置落在 `€`（占 29..32 字节）内部，Rust 的 `str` 切片在非字符边界上 panic：`byte index 30 is not a char boundary`。

**可达性**：`controller.rs:261-263` 把 `device.json` 的 `token` 原样传给 `MiioDevice::connect`，`probe-miio.rs:28` 直接用命令行参数，两条路径都没有 hex 预校验。现有测试只覆盖纯 ASCII 非法输入（`miio.rs:664-670`），测不到这条。

**后果**：worker 线程 panic → 叠加 H1 → 界面永久失效。本意是走 `MiioError::Protocol` 的正常错误分支。

**修法方向**：先 `s.is_ascii()` 或改用 `s.as_bytes().chunks(2)` 逐字节解析。

---

## 三、中危

### M1. 非原机型"故障"判定错误，46 款内置型号中 40 款诊断页故障标志恒为红色

**位置**：`crates/miac-core/src/controller.rs:683-686`、`crates/miac-app/src/main.rs:425-428`、`crates/miac-app/ui/pages/diag.slint:141-146`

```rust
// controller.rs:683（快照侧）
fault: if self.profile.model == miot::EXPECT_MODEL { miot::fault_info(fault_value) } else {
    let value = status.iter().find(|(n,_)| *n == "fault").map(|(_,v)| v);
    miot::FaultInfo { clear: value.and_then(PropValue::as_i64) == Some(0), text: ..., badge: None }
}
```
```rust
// main.rs:425（界面侧，逻辑同款）
miot::FaultInfo { clear: value.and_then(PropValue::as_i64) == Some(0), text: value.map(PropValue::display).unwrap_or_else(|| "该型号未提供故障读数".into()), badge: None }
```
```slint
// diag.slint:141,143,146 —— 颜色完全由 fault-clear 决定
background: root.fault-clear ? Theme.green.with-alpha(0.16) : Theme.red.with-alpha(0.16);
border-color: root.fault-clear ? Theme.green : Theme.red;
color: root.fault-clear ? Theme.green : Theme.red;
```

**数据核对**（本机解析 `crates/miac-core/specs/air-conditioners.json`，46 款型号）：

| fault 属性情况 | 型号数 | 判定结果 |
| --- | --- | --- |
| 无 fault 属性 | 20 | `read_props` 补 `Missing` → `as_i64()` = None → `clear=false` → **恒红**，文本显示 "—" |
| format = string | 20（非原机型） | `Value::String("0")` → `as_i64()`/`as_f64()` 均为 None → **恒红**，文本显示原始串（正常机显示红色 "0"） |
| format = uint8 | 5 | 正常（数字可判 0） |
| 原机型 `xiaomi.airc.h53h00` | 1 | 走 `faultValue`(13.1) 分支，正常 |

无 fault 属性的 20 款：`viomi.aircondition.v6~v9`、`xiaomi.aircondition.ma1~ma6/ma9/mh5/mh6`、`zhimi.aircondition.ma1~ma4/sa1/v1/v2`。
fault 为 string 的 20 款（非原机型）：`viomi.airc.m1~m4/sd26/sd27`、`xiaomi.airc.h10h00/h21h00/h25h00/h38h00/h39h00/h50h00/h51h00/h54h00/h55h00/h59h00`、`xiaomi.aircondition.mh1~mh4`。

**旁证（意图与实现冲突）**：代码专门写了 `unwrap_or_else(|| "该型号未提供故障读数")`，但 `controller.rs:417-421` 的补齐逻辑保证 `"fault"` 一定存在于状态里（`miot.rs:27` 有 `("fault", (2,3))`），所以这句提示**永远不可达**。同一文件 `main.rs:419` 处理 `clean` 属性时用的是正确写法：`if !self.profile.properties.contains_key("clean") { "该型号未提供" }`。

**修法方向**：用 `profile.properties.contains_key("fault")` 决定是否出故障卡；字符串协议（`"0"` / `"1,..."` 取首位，可复用 `maintenance()` 同款处理）纳入 clear 判定。

---

### M2. 托盘创建失败会让应用"启动即退出"（与注释承诺相反）

**位置**：`crates/miac-app/src/tray.rs:143-155`、`tray.rs:195-211`、`tray.rs:328-331`、`crates/miac-app/src/main.rs:2055-2061`

```rust
// tray.rs:151-153 —— add_icon 失败时 ? 提前返回，局部变量 tray 被 drop
let tray = Self { hwnd };
tray.add_icon(tooltip)?;     // ← LoadImageW 或 Shell_NotifyIconW(NIM_ADD) 失败
Ok(tray)
```
```rust
// tray.rs:195-207 —— Drop 里销毁窗口
impl Drop for Tray {
    fn drop(&mut self) {
        ...
        if !self.hwnd.is_null() { DestroyWindow(self.hwnd); }
```
```rust
// tray.rs:328-331 —— 窗口过程收到 WM_DESTROY 就投递退出消息
WM_DESTROY => { PostQuitMessage(0); 0 }
```

**本机实测（Python + Win32，独立进程内验证）**：对 `HWND_MESSAGE` 窗口调用 `DestroyWindow` 后，窗口过程确实收到 `WM_DESTROY`(0x2)，`PostQuitMessage(0)` 之后 `GetMessageW` **返回 0**（即 WM_QUIT）。这意味着 WM_QUIT 已被放进 UI 线程的消息队列。

**后果**：`main.rs:2101` 的 `slint::run_event_loop_until_quit()` 一启动消息泵就会取到这条 WM_QUIT 而结束 → 应用启动即退出、无窗口无提示（PE 子系统是 `windows`，连控制台都没有），而 `main.rs:2058` 的注释写的是"创建失败不影响使用"。触发条件（图标资源取不到、`Shell_NotifyIconW(NIM_ADD)` 失败，例如 Explorer 尚未就绪时开机自启）都不罕见。

**修法方向**：`WM_DESTROY` 分支只处理自己关心的清理（不要 `PostQuitMessage`），或在 `Tray::new` 失败路径上显式保留窗口句柄、不让临时的 `Tray` 走到 `Drop` 的 `DestroyWindow`。

---

### M3. 局域网→云端降级顺序错误，会造成 `link=Local ∧ local=None` 的残留状态，随后 `.expect()` panic

**位置**：`crates/miac-core/src/controller.rs:294-298`、`controller.rs:315-332`

```rust
// 294 —— 进入本地分支的前提是 link == Local
let local_result = self.local.as_mut().expect("local 链路下一定有连接").rpc(method, params.clone());
...
// 317 —— 降级：先清掉连接，再尝试云端
if self.prefer != crate::Transport::Local && self.has_cloud_session() {
    self.local = None;                              // ← 连接没了，link 还是 Local
    if self.use_cloud().is_ok() { ... }             // ← 失败时不回滚任何状态
}
return Err(local_error.into());
```

**问题**：`use_cloud()` 失败（`cloud-session.json` 在 `has_cloud_session()` 与 `read_session()` 两次独立读盘之间被改写/删除）时，`link` 仍是 `Some(ActiveLink::Local)` 而 `local` 已是 `None`。此后**任意一次 RPC**（周期快照就会触发）在 294 行的 `.expect("local 链路下一定有连接")` 上 panic。

**可达的并发来源**：扫码登录由独立线程（`login_ui`）负责，登录成功会写 `device.json` / `cloud-session.json`；而 `read_json` 每次都是"重新 `fs::read` + DPAPI 解密"，没有缓存，所以窗口真实存在。

**后果**：worker 线程 panic → 叠加 H1 → 界面永久失效。注意 `controller.rs:358-366` 反向的"云端→局域网"路径是安全的（link 只在成功时改写），只有这一条有残留。

**修法方向**：先落地 `use_cloud()` 的结果，成功后再清 `local`；失败时回滚 `link` 或直接重新 `init_local`。

---

### M4. 换设备后的"事件白名单"过滤没有超时兜底，did 一旦不匹配就永久吞掉所有事件

**位置**：`crates/miac-app/src/main.rs:581-587`、`main.rs:803`

```rust
for e in events {
    if let Some(expected_did) = self.switching_device_id.as_deref() {
        match &e {
            Event::Ready { device, .. } if device.did == expected_did => {}
            Event::NotReady { .. } | Event::Log(_) | Event::CredentialsEncrypted { .. } => {}
            _ => continue,                     // ← Snapshot / Wrote / Failed / Diag 全被丢弃
        }
    }
```
```rust
// 803 —— 扫码确认选设备后
self.switching_device_id = Some(self.creds.read_device().map(|d| d.did).unwrap_or_default());
```

**触发**：`read_device()` 返回 `None`（`device.json` 损坏或 DPAPI 解密失败）时，期望值是**空串**；或云端返回的 did 与本地记录不一致。此后任何 `Ready.did` 都不匹配。

**后果**：所有状态事件被静默丢弃，`connected` 永久 `false`，界面停在"正在连接新选择的空调…"，托盘开关机与调温全部不可用；`switching_device_id` 只在"did 完全匹配的 Ready"里被清除（全代码只有 4 处引用，无定时器兜底），**只能重启进程恢复**。

**修法方向**：给这次切换加超时（例如 10 秒）或在收到 `NotReady` 时清掉期望值。

---

### M5. 控制项在点击后会立刻回弹到旧值（模式 / 风速 / 摆风 / 定格位置 / 亮度 / 冷热效果）

**位置**：`crates/miac-app/src/main.rs:374-391`（重建 UI 值）与 `main.rs:1204-1254`、`1314-1320`、`1371-1383`（回调）

```rust
// 1204 —— 点"制冷"之后
ui.on_set_mode(move |m: i32| {
    let a = &mut *app.borrow_mut();
    a.ui.set_mode(m);                                          // ① 先画上新值
    wk.send(Command::WriteProp { name: "mode".into(), value: serde_json::json!(m) });
    a.refresh_view();                                          // ② 立刻用旧快照覆盖
});
```
```rust
// 374 —— refresh_view 无条件从"上一次快照"重建所有控件值
ui.set_mode(self.prop_i64("mode").unwrap_or(0) as i32);
ui.set_fan(self.prop_i64("fanLevel").unwrap_or(0) as i32);
ui.set_wind(self.prop_i64("windDirection").unwrap_or(0) as i32);
ui.set_vpos(self.prop_i64("verticalPos").unwrap_or(0) as i32);
ui.set_hpos(self.prop_i64("horizontalPos").unwrap_or(0) as i32);
...
ui.set_bright(self.prop_i64("lightBright").unwrap_or(0) as i32);
ui.set_effect_cool(self.prop_i64("coolingEffect").unwrap_or(0) as i32);
```

**问题**：写入是异步的，`props` 要等设备回包 + 下一次快照才会更新。回调里紧接着调用的 `refresh_view()` 会立刻把控件刷回**旧值**，用户看到"点了没反应/闪回"，要等 0.3~2 秒（设备回包 + 快照 + 下一个事件处理周期）才显示新值；若写入成功但后续快照失败，界面会一直显示旧值。

**对比证据**：温度控件专门做了在途覆盖状态机（`target_temp.rs` + `refresh_view` 里的 `self.target_temp.override_temp()`，`main.rs:323`），说明作者清楚这个问题，只是没有推广到其它控件。

**修法方向**：给这些控件也对齐"乐观值 + 在途覆盖"，或在回调里不要立即调用全量 `refresh_view()`。

---

### M6. 握手响应不校验来源地址，局域网内任意主机可污染会话（DoS / 状态欺骗）

**位置**：`crates/miac-core/src/miio.rs:346-353`（对照 `miio.rs:240-242` 的 RPC 路径）

```rust
fn recv_header_only(&mut self) -> Result<(), MiioError> {
    let mut buf = [0u8; 256];
    let (n, _from) = self.socket.recv_from(&mut buf)?;     // ← 来源地址被丢弃
    if n < 32 { return Err(...); }
    if u16::from_be_bytes([buf[0], buf[1]]) != 0x2131 { return Err(...); }
    // 356-367：无条件采纳 buf[8..12] 的 device id、buf[12..16] 的时间戳，并置 handshaked = true
```
```rust
// miio.rs:240 —— 普通 RPC 响应反而校验了来源
if from != self.addr {
    return Err(MiioError::Protocol(format!("忽略来自非目标地址 {from} 的 UDP 报文")));
}
```

**触发**：明文 hello 发出后，同网段任意主机抢在设备前回一个 ≥32 字节、前两字节 `21 31` 的 UDP 包。

**后果**：攻击者无需 token 即可写入伪造的 device id / 时间戳基准，使后续所有加密请求被真设备丢弃（局域网通道不可用），而程序认为握手成功。不能解密流量，影响限于可用性与状态欺骗，但两条路径的校验强度不一致本身就是缺陷。

---

### M7. 退出时同步等待 worker 排空指令队列，最长数十秒且托盘图标残留

**位置**：`crates/miac-app/src/main.rs:2104-2108`、`crates/miac-core/src/worker.rs:231-243`

```rust
app.borrow().settings_writer.shutdown();
worker.shutdown();          // ← 发 Shutdown 后 join()，Shutdown 排在队尾
drop(tick);
drop(tray_handle);          // ← NIM_DELETE 到这里才执行
```
worker 侧是 FIFO 逐条处理（`rx.recv_timeout(500ms)`），云端单次请求默认 8000 ms 超时（`cloud.rs:144-149`），设备离线时每条命令都要跑满超时。

**后果**：点"退出"后窗口已消失，但进程要等队列里所有命令跑完才退出（最坏数十秒）；期间托盘图标仍在、事件循环已停不再响应点击，用户观感是"退出卡死"。

---

### M8. `miac-cli regression` 的多项断言是"恒真"或"测死代码"，**通过不代表任何真实回归被覆盖**

**位置**：`crates/miac-cli/src/bin/regression.rs:151-166`、`regression.rs:168-180`、`regression.rs:73`、`regression.rs:86-93`

```rust
// 151 —— 第 7 项「属性表 40 项可解析」
for (name, _) in miac_core::miot::PROPS {
    if Controller::resolve_prop(name).is_err() { unresolved.push(*name); }
}
```
`resolve_prop` 的实现就是 `miot::PROPS.iter().find(|(n, _)| *n == name)`（`controller.rs:376-378` → `miot.rs:68-70`）。也就是说：**拿 PROPS 里的名字去 PROPS 里查自己**，按构造不可能返回 `Err`，`unresolved` 永远为空 → 这一项永远 ✅。重名、地址写错、枚举被截断，都不会被它发现。

```rust
// 168 —— 第 8 项「参数校验（5 类）」
checks.push(("温度 16~31 步长 0.5", Controller::valid_temp(26.5).is_ok() && Controller::valid_temp(26.3).is_err()));
checks.push(("模式 cool/dry/fan/heat", Controller::valid_mode(&serde_json::json!("cool")).is_ok() && ...));
...
```
这三个 `valid_*` 函数（`controller.rs:468-529`）**在生产路径上没有任何调用者**：真实的写入链路是 `set_*` → `write_prop` → `Profile::validate`（`controller.rs:451-453`、`profile.rs:153-190`），后者由型号规格驱动、随型号变化。因此这一项测的是"死代码"，改真实校验规则时它依然全绿。

**旁证（同类问题）**：`regression.rs:73`「状态快照（16 项属性）」与 `:86-93`「机器诊断（8 项）」的判据是 `missing == 0`，**条数不参与判定**（`v.len()` 只写进 detail 文本）。`read_props(&[])` 会走 `params.is_empty()` 分支返回 Ok 空表，所以 `DIAG_PROPS` / `STATUS_PROPS` 被裁剪到只剩几项甚至 0 项时，报告仍然 ✅。

**后果**：这个 regression 是项目唯一的端到端自检工具（README「开发验证」一节引用它），但它对"属性表/参数校验/属性集完整性"这三类回归**没有检测能力**，给出的是虚假信心。同文件其余项（写-回读-复位、`-5000` 记 Skip、失败 exit 1）逻辑正确。

---

### M9. `miac-cli` 读操作失败不设置退出码，脚本/CI 会把失败当成功

**位置**：`crates/miac-cli/src/main.rs:111-116`（同类见 `129`、`156`、`163`），对照 `main.rs:90-96`

```rust
// 111 —— 读状态失败只打印，函数继续返回，进程 exit 0
Err(e) => println!("读状态失败：{e}"),
```
```rust
// 90-96 —— 只有「通道建立失败」才会 exit(1)
```
**后果**：`miac-cli status` 在设备离线时输出一行错误文本并以 **0** 退出，任何用它做健康检查/自动化的脚本都会误判。同一仓库的 `regression` 对同类失败是 ❌ + `exit 1`，两套口径不一致。

**修法方向**：读失败走 `eprintln!` + `std::process::exit(1)`，与 `regression` 统一。

---

## 四、低危 / 健壮性 / 工程卫生

| # | 位置 | 问题 | 影响 |
| --- | --- | --- | --- |
| L1 | `lib.rs` 侧 `login.rs:508-521` | `now_iso8601()` 混用**本地日期**（`ymd_from_unix` 在 Windows 上走 `SystemTimeToTzSpecificLocalTime`）与 **UTC 时刻**（`secs % 86400`）再拼 `…Z` | `savedAt` 差一个时区偏移（UTC+8 差 8 小时），跨零点时日期与时刻自相矛盾；与 v1 的 `new Date().toISOString()` 不一致 |
| L2 | `miio.rs:396-397` | `rpc` 的"按 id 匹配、最多读 6 个包"循环里用的是 `recv_packet()?`，任何畸形包（来源不符/校验和不符/长度不符）都会立即终止本次 RPC，而 id 不匹配却被正确跳过 | 获知本地临时端口的任何主机可低成本打断通信；策略内部不一致 |
| L3 | `settings.rs:107-111` | `credits.read_json::<Settings>().unwrap_or_default()`：serde 对结构体是整体失败，任一字段类型不符（`"dark":1`、`"transport":"auto"`）即整份回默认，而 `SettingsWriter` 下一次保存会用默认值覆盖原文件；`migrated` 同时被重置会触发重新迁移 | 主题/预设/通道/关闭方式全部丢失且不可恢复（非凭据） |
| L4 | `crypto.rs:76-99` | `rc4` 的 `key[i % key.len().max(1)]` 只防除零，空 key 时 `key[0]` 越界 panic | 当前无生产调用点（仅有定义与测试），属 API 隐患 |
| L5 | `miio.rs:199-202` | `let total_len = (32 + encrypted.len()) as u16;` 无上限检查 | 当前调用方构造不出 >65 KB 的 payload，不可达；`rpc` 是 `pub` API，将来传入大 Value 会写出错误长度 |
| L6 | `miot.rs:282-285` | `value.abs()` 对 `i64::MIN` 溢出 | debug 构建 panic，release 显示"未知代码"且按已清除处理；需要设备返回该极端值 |
| L7 | `tls.rs:41-47` | 测试断言 `cfg.alpn_protocols.len() <= 8` 恒真（ALPN 默认空 Vec），与该测试声称的"根证书已加载"无关 | 即使 `roots.extend(...)` 被改成空集，测试仍绿 |
| L8 | `credentials.rs:314` + `dpapi.rs:156-163` | 明文判定用 `serde_json::from_slice`（不接受 BOM），带 UTF-8 BOM 的明文凭据会被判 `Unreadable`：既读不到也永远不被升级；专门识别 BOM 的 `looks_encrypted` 全仓只有测试调用（死代码） | 旧的/外部工具写出的 BOM 凭据让程序要求重新登录 |
| L9 | `credentials.rs:313-323` | 只擦除了中间缓冲 `raw`/`plain`，serde 反序列化出的 `DeviceInfo.token` / `CloudSession.service_token` 是独立堆分配，既不擦除也不清零 | 与项目自述威胁模型（不防同用户进程）一致，属纵深防御缺口 |
| L10 | `credentials.rs:199-212` | "回退目录"包含**可执行文件目录**与**当前工作目录**，同名 JSON 会被 `upgrade_existing()` / `migration::run()` 加密固化进 `%APPDATA%\米家空调\` 并长期生效 | 凭据来源混淆：可能连到放置者控制的设备/会话（不泄露自身 DPAPI 凭据） |
| L11 | `login.rs:428-433` | 轮询失败（DNS 立即失败、代理不可用）无退避/睡眠，在 `expires_at` 前最长 300 秒无间歇重试 | 忙等 + 请求放大 |
| L12 | `login.rs:439-444` | 拿到 200 但没有 `location` 即判"二维码已失效"，用"字段是否存在"代替业务 code 判定 | 边界情形下二维码还有效却要求重新生成（需真机抓包确认触发条件） |
| L13 | `login.rs:350-356`（`redirects(0)` 见 279） | 下载二维码的 `get_bytes` 不处理 3xx（同文件 `request()` 是手动逐跳处理的） | 若 qr 端点返回跳转，会因空 body 报"不是有效图片"，误导排查 |
| L14 | `cloud.rs:159-164` vs `login.rs:282` | 无效代理时前者把代理串原文（可能含 `user:pass@`）打进 stderr，后者完全静默直连 | 凭据进日志 / 用户不知道代理没生效 |
| L15 | `cloud.rs:350-353`、`375-378`、`389-392`、`401-409` | `result` 形状不符（对象/字符串/null）时静默返回空列表 | 把"响应异常"伪装成"没有数据"（"没有耗电数据"/"账号下没有设备"），无日志 |
| L16 | `login.rs:501-505` | 设备列表里 `did` 非字符串的条目被 `filter_map` 静默丢弃 | 全部被过滤时 UI 只说"没有读到任何设备"，无法区分"真没设备"与"解析全失败" |
| L17 | `controller.rs:737-745` | 温湿度计历史回退算出了 `updated`（最大 updateTime）却从未使用，且 `device_data(..., now-7d, now+60, 1)` 只取第一条 | 若云端按时间升序返回，7 天前的读数会被当成当前室温展示 |
| L18 | `profile.rs:116-122` + `controller.rs:552-556` | fan-level 只有 `value-range` 的 4 款型号（`viomi.aircondition.v6~v9`）合成的档位标签是数字字符串，而 `set_fan("auto")` 只找标签 `"自动"` | 这 4 款上 `fan auto` 必然失败，而同函数 `max` 却能成功，行为自相矛盾 |
| L19 | `controller.rs:468-506` | `valid_temp` 硬编码 16~31/0.5、`valid_mode` 硬编码 `cool=2`、`valid_fan("max")=8`，与按型号加载的规格冲突。**规格表实测**：46 款中 12 款温度上限是 **32**（`viomi.airc.chacru/sd25`、`xiaomi.aircondition.ma7/ma8`、`zhimi.*` 全部），6 款步长是 **0.1**（`zhimi.aircondition.ma1/ma2/ma4/sa1/v1/v2`）；模式编号有 6 种不同组合，其中 4 款是 `cool=1/dry=2/fan=3/heat=4`（硬编码会把 cool 写成 2 = dry） | 当前只被单元测试与 `miac-cli regression` 使用，写路径走 `profile.validate` + `snap_temperature`，**尚未产生错误控制**，属误用陷阱 |
| L20 | `main.rs:1019` + `tray.rs:214-241` | `attach_main_window(std::ptr::null_mut())` 之后仍对空 HWND 调 `SendMessageW(hwnd, WM_SETICON, …)`（只判了图标句柄、没判窗口句柄） | 无效调用 + 每次白做两次 `LoadImageW`（"NULL 等同广播"未获文档支持，倾向不成立） |
| L21 | `settings_writer.rs:57`、`login_ui.rs:81` | 线程创建失败用 `.expect()` 在 UI 线程 panic | `windows` 子系统下无任何提示，进程直接消失 |
| L22 | `main.rs:599`、`main.rs:699` | 清理待应用温度的关键保存用 `let _ = save(...)` 吞掉 Result（其它调用点都有 toast/eprintln） | 保存线程已退出时静默丢更新，下次启动可能给错误设备下发温度 |
| L23 | `login_ui.rs:121-130` | PNG 尺寸上限校验（>2048 判非法）发生在 `vec![0u8; reader.output_buffer_size()?]` **之后**（已逐字核验：分配在 124 行、校验在 128 行） | 声明超大尺寸的 PNG 会先按声明尺寸请求分配，失败即 Rust alloc abort（进程直接死），成功则瞬时占用按声明尺寸计算的内存；`png` 的 `Limits::bytes`（默认 64 MiB）是否在 `read_info()` 阶段就拦住尺寸炸弹，本机无法验证 —— 若它生效则本条降为"顺序不优雅"，若不生效则是可利用的内存耗尽点 |
| L24 | `main.rs:2066-2088` | `attach_timer` 的闭包持有自己的 `Rc<Timer>`，同时每个 `ui.on_*` 闭包持有 `Rc<RefCell<App>>` 而 `App` 持有 `MainWindow` | 强引用循环使 `Drop` 全不执行（正常退出靠显式 `shutdown()` 兜住；`--self-test` 分支直接 return，完全跳过 shutdown 与设置 flush） |
| L25 | `tray.rs:158-192` | 未注册 `TaskbarCreated` 消息 | Explorer 重启后托盘图标永久消失；若关闭行为是"收进托盘"，用户会以为程序已退出 |
| L26 | `single_instance.rs:26-44` | 先 `CreateEventW` 后 `CreateMutexW`：两个调用之间若被后启动的进程抢先创建互斥体，先启动的实例会拿到 `ERROR_ALREADY_EXISTS` 而自行退出，由后启动者当主实例（互斥体语义仍保证系统内只有一个，用户基本无感，窗口在微秒级） | 启动竞态下的主实例归属颠倒；`Local\` 为会话级命名空间，跨会话（RDP）可并存属设计选择 |
| L27 | `skills/miac-control/scripts/` | `miac-app.exe`(12.6 MB) 与 `miac-assistant.exe`(2.5 MB) 被提交进仓库，`.gitignore` 只排除了 `/dist/*.exe`，未覆盖 `skills/` | 仓库体积 +15 MB；二进制与源码版本可能不同步，且无法审查 |
| L28 | `dist/启动空调.cmd` | 文件是 **GBK(936)** 编码（`c3 d7` = "米"），仓库其余文本均为 UTF-8 | 在非 936 代码页环境（含"UTF-8 全球语言支持"beta）下中文注释乱码，命令本身为 ASCII 故暂不影响执行 |
| L29 | `miac-cli/src/main.rs:166-170` | 未知子命令会**先真正建立通道**（`88-96` 的 `init_transport`）再打印用法提示，且 `exit 0` | 打错一个字就会连一次设备；与 `skills/miac-control/src/main.rs:75` 同类情况直接返回 `Err` 不一致 |
| L30 | `miac-cli/src/main.rs:158-164` | `raw` 缺参或参数非数字时静默回退成读 `2.1`（`unwrap_or(2)` / `unwrap_or(1)`） | 读错目标却按成功返回；`regression.rs` 里 `raw 2 1` 是有效命令，容易掩盖手误 |
| L31 | `miac-cli/src/main.rs:80-87` | `--cloud --local` 同时出现时静默选云端 | 与 `skills/miac-control/src/main.rs:32-34` 对同一组合**报硬错误**的行为相反 |
| L32 | `miac-cli/src/main.rs:191-193`、`232-248` | 登录流程把二维码写到 **CWD** 的 `login-qr.png/jpg`，中途失败（写云会话失败、拉设备失败）直接 `return`，只有成功或"等确认失败"才删；`.gitignore` 无 `login-qr*` 规则 | `git add -A` 会把二维码（含会话上下文）带进仓库 |
| L33 | `regression.rs:196-207` | `--write` 复位用 `let _ = ctrl.set_toggle(name, orig)`，错误被丢弃且无复位回读，detail 仍写"已复位" | 真实设备可能停在改动状态，与"避免留下副作用"的承诺不符 |
| L34 | `miac-app/build.rs:43`（配合 21、39） | `find_rc().expect("未找到 Windows SDK 的 rc.exe…")`，只在 `%WindowsSdkDir%\bin\<ver>\x64` 与 `C:\Program Files (x86)\Windows Kits\10\bin\<ver>\x64` 两处找 | 缺 SDK 时 `cargo check/test/build` 全部 panic，没有开关或降级（对比：托盘创建失败是有降级的） |
| L35 | `ui/pages/debug.slint:55-62` | 高级属性页的选择器硬编码 `for i in 40`，而 `main.rs:905` 用 `profile.properties.keys()` 填充名字列表 | 当前 `miot::PROPS` 恰好 40 项，刚好卡在边界；一旦属性表增到 41 项，多出来的属性在界面上直接消失（无报错） |
| L36 | `清理米家空调数据.cmd:61-62` | 第 60 行注释写"Exact filenames only"，第 62 行实际按通配符删除 `%~1\*.log` 与 `.*.tmp` | 超出"仅清理米家空调数据"的自述范围：仓库根/`dist`/`target` 下的无关 `.log` 也会被删（当前仓库无同名文件，未造成实际损失） |
| L37 | `scripts/update-miot-specs.mjs:43-47` | 完整性校验正确（失败即抛错、不覆盖），但第 47 行 `writeFile` **非原子**且用相对路径 | 写入中断会留下截断的 `air-conditioners.json`，而它经 `include_str!` + `expect("invalid bundled MIoT specs")`（`profile.rs:270-273`）在下次构建/启动时直接 panic；且必须在仓库根执行 |
| L38 | `skills/miac-control/src/main.rs:165-181` | `write_result` 恒返回 `ok:true` / `acknowledged:true`（语义 = "下发已提交"），`verified` 才代表回读生效，且 `verified:false` 时进程仍 `exit 0` | 语义自洽且 `SKILL.md:36` 已明确要求检查四个字段，但只读 `ok` 的调用方会把"未确认"当成功 |

---

## 五、已逐项核验、判定无问题的区域

为免重复排查，以下方向都查过并有结论：

- **miIO 协议正确性**：hello 包 32 字节全 0xFF 布局、`MD5(header[0..16] ‖ token ‖ 密文)` 校验和覆盖范围、大端序、握手前 `0xFFFFFFFF` 哨兵、`stamp` 回绕用 `wrapping_add`、密钥 `MD5(token)` 与 IV `MD5(key‖token)` 的派生 —— 发送/接收对称且与参考实现一致。`crypto.rs:48` 的 `chunk.try_into().unwrap()` 因前置 `len % 16` 检查不会 panic。
- **小米云签名**：nonce（8 随机 + 4 字节大端分钟）、`signedNonce = SHA256(base64decode(ssecurity) ‖ nonce)`、`signature = HMAC-SHA256(key=原始 signedNonce, msg=path & sn_b64 & nonce_b64 & k=v…)`、form-urlencoded 字段、Cookie 7 项、签名路径不含 `/app` —— 与 node-mihome / python-miio 逐条一致；随机源为 `OsRng`；"签名用 data"与"发送 data"是同一字符串。
- **凭据链路（安全关键）**：DPAPI 传参与 `LocalFree` 配对、失败分支无泄漏；`create_new + write_all + sync_all` 后先 `icacls` 临时文件再 `rename`（同目录原子替换），失败即删临时文件且**没有**任何"降级为明文/宽松权限"的分支；ACL 用真实令牌 SID（非 `USERNAME` 环境变量），**本机实测** `icacls <file> /inheritance:r /grant:r *SID:(F)` 语法有效（结果为只剩当前用户 `(F)`）；文件名白名单防路径穿越；旧的旧版明文副本不擅自删除（与 README 声明一致）。
- **温度链路**：`snap_temperature` + `validate` 对 4 种真实 range（16-31/0.5、16-32/0.1、16-32/0.5、16-32/1）各 2000 点穷举，零越界、零误拒；写入防重发有效（`rpc_retry_safe` 只放行 `get_properties`，设备拒绝与写超时都不换通道重发）。
- **模式写入禁用**：`ma1/ma2/ma4` 与"同级多版本"两条路径都覆盖，`validate` 再拦一次。
- **`read_property_batches`**：分批 + 二分恢复逻辑正确，`first_error.expect(...)` 不可达（params 非空必执行过 read；空 params 走 `is_empty()` 早退）。
- **日期算法**：`ymd_from_unix_utc` 与 `first_weekday_of_month` 用 Python 在 1900–2100 / 公元 1–9999 年范围内穷举比对 Python `datetime`，**零误差**（真正的问题只在 L1 的时区混用）。
- **Slint 借用重入**：用 `try_borrow_mut` + `pending` 暂存规避了"属性变更内同步跑回调导致借用 panic / 事件丢失"的真坑，注释里记录了踩坑经过。
- **`log_export.rs`**：`OPENFILENAMEW`（`lStructSize`、`nMaxFile=32768`、NUL 结尾、取消 vs 错误分支）用法正确。
- **`target_temp.rs`**：步长/上下限/settle 300 ms/失败回滚/快照门禁逻辑自洽，边界测试完备。
- **非测试代码**：`miac-app` 侧无 `unwrap/expect/panic!/process::exit`、无索引越界；`miac-core` 侧 `unwrap/expect` 全部在 `#[cfg(test)]` 内（例外是 worker 的 `expect("有控制器")`，当前不变式成立）。
- **Slint 界面**：`power.slint:46` 的 `energy-milli / Math.max(max-milli,1)` 是**浮点除法不是整除**（Slint 编译器的 `ArithmeticOp` 分支 `(false,false) => Type::Float32`，见 slint 1.13 `internal/compiler/passes/resolving.rs`；并用随仓库的 `docs-slint-ui/v2-2-power.png` 做像素采样，与 `alpha = 0.10 + 0.45t` 的浮点模型逐像素吻合）；`dial-math.slint` 无 `acos/atan`，`ratio` 有 `max<=min` 守卫、`tick-angle` 有 `count<=1` 守卫、拖拽两端都 clamp，NaN 不可达；`main.slint` 的五页分派所有 `set_view` 调用点（`main.rs:1102` 导航、`2037` 的 `--tour` 带 `(0..5).contains` 校验、`2140` 的 `n % 5`）都在 `0..4` 内，不会漏显示。
- **注入面**：Skill/CLI 唯一的子进程调用是 `rundll32.exe url.dll,FileProtocolHandler <路径>`（固定参数向量、不经 shell）；所有写操作都过 `Profile::validate`；未知型号在 `profile::load` 直接返回 `Err`，不会落到宽松默认。
- **构建脚本**：`build.rs` 的增量构建无缺陷（`slint_build` 自身会为所有编译到的 `.slint` 打印 `rerun-if-changed`，因此漏列 `ui/login.slint`、`ui/close-dialog.slint` 无影响）；`probe-miio.rs` 的 hello 头、`key = MD5(token)` / `iv = MD5(key‖token)`、校验和范围均正确且切片都带长度守卫。
- **`--write` 以外的 regression 项**：写-回读-复位、`-5000` 记 Skip、失败 `exit 1` 的逻辑正确（问题只在中危 M8 列出的那几项）。

---

## 六、验证方式与限制

**做过的实证**
1. Win32 消息链：Python + ctypes 在本机创建 `HWND_MESSAGE` 窗口，实测 `DestroyWindow` → `WM_DESTROY`(0x2) → `PostQuitMessage` → `GetMessageW` 返回 0（支撑 M2）。
2. `icacls <file> /inheritance:r /grant:r *SID:(F)` 实际执行并复核 ACL 结果（排除"凭据加固必然失败"的误判）。
3. 日期算法：Python 重实现后与 `datetime` 全范围比对（排除误报）。
4. 规格表数据：Python 解析 `specs/air-conditioners.json` 46 款，统计 fault 属性格式分布（支撑 M1 的 20+20 数字）。
5. 上游一致性：逐字节比对本地副本与 GitHub `main` 的 `controller.rs` / `profile.rs` / `miot.rs` / `worker.rs` / `settings.rs` / `target_temp.rs`，全部 identical。

**限制**
- 无 Rust 工具链，**未编译、未跑 `cargo test`、未跑 clippy**；因此不包含编译期问题，也无法通过测试回归来验证上述行为。
- 涉及真机/真账号的路径（扫码登录轮询响应形状、miIO 响应明文是否带 16 字节 token 前缀、云端历史数据返回顺序）标注为待验证，均给出了验证方法。
- 行号基于 `main` 分支当前快照；若后续提交移动了代码，请按函数名检索。

**一条待验证项（不列入正式缺陷）**：`miio.rs:285-299` 的 JSON 提取取"第一个 `{`"。代码自身的注释（285 行）与测试 `json_extraction_skips_leading_token`（`miio.rs:754-770`）都声称响应明文可能是"16 字节 token + JSON"。若该前缀真实存在（测试用 `0xAA` 填充所以不会暴露问题），则 token 的 16 字节中只要含一个 `0x7B`（概率约 6%）起始点就会落在 token 内部，导致该设备的响应解析持续失败。**验证方法**：真机抓一次 `get_properties` 响应，看解密后的第 17 字节是否为 `{`；或在 dev 构建里把 token 换成含 `0x7B` 的值跑一次。
