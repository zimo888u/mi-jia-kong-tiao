// ─────────────────────────────────────────────────────────────────────────────
// tray.rs —— 系统托盘图标（Win32 Shell_NotifyIcon，单进程）
//
// ## 为什么不用 tray-icon / muda 这类 crate
//
// 它们的 Windows 后端要么额外拉一个事件线程，要么把 GTK/objc 生态带进来，
// 与「单进程 + 25–45 MB」的验收目标相冲突。这里直接用 Win32：
//
//   - 在 **UI 线程**上建一个「消息专用窗口」（HWND_MESSAGE），
//     它的消息队列由 Slint/winit 已有的消息泵派发，不需要新线程；
//   - 用 Shell_NotifyIconW 把图标挂到通知区；
//   - 回调统一走 WM_APP+1，WndProc 里只做一件事：PostMessage 一条命令给自己，
//     真正的处理放在界面的定时器里（见 main.rs 的 drain_tray_commands）。
//
// ## 为什么用 PostMessage 传命令而不是共享队列
//
// WndProc 运行在消息泵里，此时界面状态正被借用，直接回调易踩借用冲突。
// 改成「投递消息 → 定时器里取」之后，所有状态修改都发生在同一个地方，
// 逻辑简单且不会有重入问题。
// ─────────────────────────────────────────────────────────────────────────────

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, SetWindowSubclass, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
    NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, LoadImageW, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
    ShowWindow, TrackPopupMenu, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED, MF_SEPARATOR, MF_STRING,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP,
    WNDCLASSW, SW_HIDE,
};

/// 托盘回调用的自定义消息（图标事件都发到这个消息上）。
const WM_TRAYICON: u32 = WM_APP + 1;

/// 托盘图标在通知区里的 ID。
const TRAY_ID: u32 = 1;

/// 菜单项命令 ID。
const CMD_SHOW: usize = 1001;
const CMD_TOGGLE_POWER: usize = 1002;
const CMD_EXIT: usize = 1003;

/// 消息专用窗口的窗口类名（宽字符、以 NUL 结尾）。
const CLASS_NAME: &[u16] = &[
    b'M' as u16, b'i' as u16, b'a' as u16, b'c' as u16, b'T' as u16, b'r' as u16, b'a' as u16,
    b'y' as u16, 0,
];

/// 界面要执行的托盘命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// 显示主窗口
    Show,
    /// 开关机（切换）
    TogglePower,
    /// 退出程序
    Exit,
}

/// 待处理命令队列。
///
/// ## 为什么用队列而不是「PostMessage 给自己 + PeekMessage 取」
///
/// 早先的实现是：WndProc 收到点击 → `PostMessageW(hwnd, WM_COMMAND, …)`，
/// 界面定时器再用 `PeekMessageW` 取。结果**取不到**——`PeekMessage` 会把消息
/// 留在队列里（PM_NOREMOVE 语义）然后又进了一次 WndProc，那条分支并不真正
/// 处理，消息就这么被吞了，托盘菜单点了没反应。
///
/// 改成 WndProc 事件里直接压队列、定时器直接弹队列：不依赖消息泵的时序，
/// 也不会被别的消息干扰。队列用 Mutex 保护是因为 Shell 的回调理论上可能
/// 从别的线程进来。
static COMMANDS: Mutex<Vec<TrayCommand>> = Mutex::new(Vec::new());

/// 压一条命令（WndProc 与菜单回调里调用）。
fn queue_push(cmd: TrayCommand) {
    if let Ok(mut q) = COMMANDS.lock() {
        q.push(cmd);
    }
}

/// 托盘图标是否已注册（避免重复 Add / 重复 Delete）。
static ICON_ADDED: AtomicBool = AtomicBool::new(false);

/// 「关闭按钮 = 收进托盘」是否启用（由界面设置决定）。
static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(true);

/// 消息专用窗口句柄（0 表示尚未创建）。
static mut MSG_HWND: HWND = std::ptr::null_mut();

/// 主窗口句柄的副本（用于「关闭时隐藏」与「左键切回」）。
static mut MAIN_HWND: HWND = std::ptr::null_mut();

/// 托盘控制器。
pub struct Tray {
    hwnd: HWND,
}

impl Tray {
    /// 在**当前线程**创建托盘。必须在 UI 线程调用（消息专用窗口要在同一线程，
    /// 这样它的消息才会被 Slint/winit 的消息泵派发）。
    pub fn new(tooltip: &str) -> Result<Self, String> {
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());

            // 注册窗口类（重复注册同一个类名会失败，忽略即可）
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wnd_proc);
            wc.hInstance = hinstance;
            wc.lpszClassName = CLASS_NAME.as_ptr();
            RegisterClassW(&wc);

            // 消息专用窗口：HWND_MESSAGE = (HWND)-3，不可见、不参与绘制，
            // 只用来接收 Shell_NotifyIcon 的回调消息。
            let hwnd = CreateWindowExW(
                0,
                CLASS_NAME.as_ptr(),
                CLASS_NAME.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                -3isize as HWND,
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return Err(format!(
                    "创建托盘消息窗口失败（GetLastError={}）",
                    std::io::Error::last_os_error()
                ));
            }
            MSG_HWND = hwnd;

            let tray = Self { hwnd };
            tray.add_icon(tooltip)?;
            Ok(tray)
        }
    }

    /// 往通知区添加图标。
    fn add_icon(&self, tooltip: &str) -> Result<(), String> {
        unsafe {
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = self.hwnd;
            nid.uID = TRAY_ID;
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_TRAYICON;

            // 用可执行文件自带的图标，不依赖外部 .ico 文件
            let hinstance = GetModuleHandleW(std::ptr::null());
            let icon = LoadImageW(
                hinstance,
                std::ptr::null(), // 用 exe 的默认图标
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE | LR_SHARED,
            );
            nid.hIcon = icon;

            // 提示文字：截断到 127 个宽字符（NOTIFYICONDATAW.szTip 容量 128）
            let tip: Vec<u16> = tooltip.encode_utf16().take(127).chain([0]).collect();
            nid.szTip[..tip.len()].copy_from_slice(&tip);

            if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
                return Err("Shell_NotifyIcon(NIM_ADD) 失败".into());
            }
            ICON_ADDED.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            if ICON_ADDED.swap(false, Ordering::SeqCst) {
                let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = self.hwnd;
                nid.uID = TRAY_ID;
                Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            if !self.hwnd.is_null() {
                DestroyWindow(self.hwnd);
            }
            MSG_HWND = std::ptr::null_mut();
        }
    }
}

/// 把主窗口与本模块绑定：
///   - 记录 HWND（托盘左键切回、关闭时隐藏都要用）
///   - 挂钩窗口子类，把「关闭」改成「隐藏到托盘」
///
/// 用 `SetWindowSubclass` 而不是 `SetWindowLongPtr(GWLP_WNDPROC)`：
/// 前者维护子类链，`DefSubclassProc` 会自动把消息交还给原来的 winit 窗口过程，
/// 不需要自己保存/转发旧过程指针（那样很容易把 winit 的消息循环搞坏）。
pub fn attach_main_window(hwnd: HWND, close_to_tray: bool) {
    unsafe {
        if hwnd.is_null() {
            return;
        }
        MAIN_HWND = hwnd;
        CLOSE_TO_TRAY.store(close_to_tray, Ordering::SeqCst);
        SetWindowSubclass(hwnd, Some(close_interceptor), 1, 0);
    }
}

/// 主窗口的子类过程：只拦 WM_CLOSE。
///
/// 启用「收进托盘」时把关闭变成隐藏——窗口与进程都留着，托盘图标继续可用，
/// 这正是「托盘待机」的内存口径。其余消息一律交给 `DefSubclassProc` 原样处理，
/// 绝不影响 winit 自己的逻辑。
unsafe extern "system" fn close_interceptor(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_CLOSE && CLOSE_TO_TRAY.load(Ordering::SeqCst) {
        // 收进托盘必须真正隐藏窗口；仅压到最底层仍会出现在任务栏、Alt+Tab，
        // 且桌面无遮挡时可见。
        //
        // 为什么不隐藏（这一段是实测换来的，别轻易改回去）：
        // 窗口一旦 SW_HIDE，客户端区域不再映射，Slint 的软件渲染后端回来后
        // 不重画——恢复后整屏纯白（PrintWindow 取证：正常 150+ 色 → 恢复后 12 色）。
        // 以下补救**全都无效**：
        //   · window().request_redraw()（连续多拍请求）
        //   · InvalidateRect + UpdateWindow
        //   · RedrawWindow（无效化 + 擦除 + 子窗口 + 立即）
        //   · MoveWindow 尺寸往返（同样的调用从外部脚本发有效，进程内发无效）
        //   · 移到屏幕外（窗口仍「可见」，但渲染同样停掉，恢复后还是白）
        //
        // 而「压到底层」不同：窗口始终可见、始终参与合成，渲染表面一直有效。
        // 实测遮挡 5 秒再拉回顶层，PrintWindow 仍是 153 色，画面完好。
        hide_to_back(hwnd);
        return 0; // 已处理：不往下传，窗口不会被销毁
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

/// 真正隐藏窗口。恢复路径由 main.rs 先调用 Slint 的 window().show()，再补 Win32 置前和重绘。
pub fn hide_to_back(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
        trace("hide_to_back");
    }
}

/// 取出所有待处理的托盘命令（界面定时器里调用）。
pub fn drain_commands() -> Vec<TrayCommand> {
    match COMMANDS.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => Vec::new(),
    }
}


/// 显示并前置主窗口。
pub fn show_main_window() {
    unsafe {
        let h = MAIN_HWND;
        if h.is_null() {
            return;
        }
        // 「收进托盘」是压到底层（不是隐藏），所以恢复就是拉回顶层 + 前置。
        // 全程窗口都处于可见/已合成状态，渲染表面有效，不需要任何重绘兜底。
        use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos;
        const FLAGS: u32 = 0x0001 | 0x0002 | 0x0040; // NOSIZE|NOMOVE|SHOWWINDOW
        SetWindowPos(h, std::ptr::null_mut(), 0, 0, 0, 0, FLAGS);
        trace("show_main_window");
    }
}

/// 诊断用：设了 `MIAC_TRACE` 环境变量时，把关键动作追加写到该文件。
///
/// 托盘这类交互在自动化里很难从外部观察——窗口到底有没有重画，只能靠截图
/// 间接判断，而截图本身又有时序噪声。留一个可开关的落盘日志，排查省事。
fn trace(msg: &str) {
    let Ok(path) = std::env::var("MIAC_TRACE") else {
        return;
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[tray] {msg}");
    }
}

/// 托盘窗口过程：只做「把命令投递给自己」这一件事。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event = lparam as u32;
            match event {
                WM_LBUTTONUP => {
                    // 左键：显示并前置主窗口
                    queue_push(TrayCommand::Show);
                }
                WM_RBUTTONUP => {
                    show_menu(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 右键菜单。
///
/// TrackPopupMenu 有个 Windows 的老规矩：菜单弹出前必须先
/// `SetForegroundWindow`，否则点菜单外面菜单不会消失（会一直挂着）。
unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let text = |s: &str| -> Vec<u16> { s.encode_utf16().chain([0]).collect() };

    AppendMenuW(menu, MF_STRING, CMD_SHOW, text("显示主界面").as_ptr());
    AppendMenuW(menu, MF_STRING, CMD_TOGGLE_POWER, text("开机 / 关机").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, CMD_EXIT, text("退出").as_ptr());

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);

    SetForegroundWindow(hwnd);
    // TPM_RETURNCMD：直接返回被点的命令 ID，省掉 WM_COMMAND 转发
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    // 菜单关闭后把焦点还给主窗口，否则焦点会留在托盘上
    if !MAIN_HWND.is_null() {
        PostMessageW(MAIN_HWND, 0, 0, 0);
    }
    DestroyMenu(menu);
    SetFocus(hwnd);

    // TPM_RETURNCMD 让 TrackPopupMenu 直接返回被点的命令 ID，
    // 这里转成队列命令即可（不再走 WM_COMMAND 转发）
    match cmd as usize {
        CMD_SHOW => queue_push(TrayCommand::Show),
        CMD_TOGGLE_POWER => queue_push(TrayCommand::TogglePower),
        CMD_EXIT => queue_push(TrayCommand::Exit),
        _ => {}
    }
}
