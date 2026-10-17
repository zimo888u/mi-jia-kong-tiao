// ─────────────────────────────────────────────────────────────────────────────
// tray.rs —— 系统托盘图标（Win32 Shell_NotifyIcon，单进程）
//
// ## 为什么不用 tray-icon / muda 这类 crate
//
// 它们的 Windows 后端要么额外拉一个事件线程，要么把 GTK/objc 生态带进来，
// 与「单进程 + 25–45 MB」的验收目标相冲突。这里直接用 Win32：
//
//   - 在 **UI 线程**上建一个隐藏的顶层辅助窗口，
//     它的消息队列由 Slint/winit 已有的消息泵派发，不需要新线程；
//   - 用 Shell_NotifyIconW 把图标挂到通知区；
//   - 图标回调统一走 WM_APP+1，WndProc 将用户命令入队，
//     真正的处理放在界面的定时器里（见 main.rs 的 drain_commands）。
//
// ## 为什么把用户命令交给定时器执行
//
// WndProc 运行在消息泵里，此时界面状态正被借用，直接回调易踩借用冲突。
// 改成「命令入队 → 定时器里取」之后，所有状态修改都发生在同一个地方，
// 逻辑简单且不会有重入问题。
// ─────────────────────────────────────────────────────────────────────────────

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
    NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, LoadImageW, PostMessageW, RegisterClassW, SendMessageW,
    GetWindowLongPtrW, RegisterWindowMessageW, SetForegroundWindow, SetWindowLongPtrW, GWLP_USERDATA,
    TrackPopupMenu, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED, MF_SEPARATOR, MF_STRING,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP,
    WNDCLASSW,
};

/// 托盘回调用的自定义消息（图标事件都发到这个消息上）。
const WM_TRAYICON: u32 = WM_APP + 1;

/// 托盘图标在通知区里的 ID。
const TRAY_ID: u32 = 1;

/// `app-icon.rc` 中的应用图标资源 ID。
const APP_ICON_ID: usize = 1;
const WM_SETICON: u32 = 0x0080;
const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;

/// 菜单项命令 ID。
const CMD_SHOW: usize = 1001;
const CMD_TOGGLE_POWER: usize = 1002;
const CMD_EXIT: usize = 1003;

/// 隐藏辅助窗口的窗口类名（宽字符、以 NUL 结尾）。
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


// 主窗口句柄只在当前 UI 线程访问。
thread_local! {
    static MAIN_HWND: std::cell::Cell<HWND> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

struct TrayWindowState {
    tooltip: String,
    taskbar_created: u32,
    add_icon: fn(HWND, &str) -> Result<(), String>,
}

/// 托盘控制器。
pub struct Tray {
    hwnd: HWND,
    // The boxed address remains stable until the native window is destroyed.
    _state: Box<TrayWindowState>,
}

impl Tray {
    /// 在**当前线程**创建托盘。必须在 UI 线程调用（辅助窗口要在同一线程，
    /// 这样它的消息才会被 Slint/winit 的消息泵派发）。
    pub fn new(tooltip: &str) -> Result<Self, String> {
        unsafe {
            let message: Vec<u16> = "TaskbarCreated".encode_utf16().chain([0]).collect();
            let taskbar_created = RegisterWindowMessageW(message.as_ptr());
            if taskbar_created == 0 {
                return Err(format!("注册任务栏恢复消息失败：{}", std::io::Error::last_os_error()));
            }
            let mut state = Box::new(TrayWindowState {
                tooltip: tooltip.to_owned(), taskbar_created, add_icon,
            });
            let hwnd = create_tray_window(&mut *state)?;
            let tray = Self { hwnd, _state: state };
            add_icon(hwnd, tooltip)?;
            Ok(tray)
        }
    }
}

unsafe fn create_tray_window(state: &mut TrayWindowState) -> Result<HWND, String> {
    let hinstance = GetModuleHandleW(std::ptr::null());
    let mut wc: WNDCLASSW = std::mem::zeroed();
    wc.lpfnWndProc = Some(wnd_proc);
    wc.hInstance = hinstance;
    wc.lpszClassName = CLASS_NAME.as_ptr();
    RegisterClassW(&wc);
    // HWND_MESSAGE windows never receive TaskbarCreated broadcasts. A hidden
    // top-level window remains invisible while participating in those notices.
    let hwnd = CreateWindowExW(0, CLASS_NAME.as_ptr(), CLASS_NAME.as_ptr(), 0,
        0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), hinstance, std::ptr::null());
    if hwnd.is_null() {
        return Err(format!("创建托盘消息窗口失败：{}", std::io::Error::last_os_error()));
    }
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as *mut TrayWindowState as isize);
    Ok(hwnd)
}

/// 往通知区添加图标。
fn add_icon(hwnd: HWND, tooltip: &str) -> Result<(), String> {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;

        // 用可执行文件自带的图标，不依赖外部 .ico 文件
        let hinstance = GetModuleHandleW(std::ptr::null());
        let icon = LoadImageW(
            hinstance,
            APP_ICON_ID as *const u16,
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        );
        if icon.is_null() {
            return Err("读取内嵌应用图标失败".into());
        }
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
        }
    }
}

/// 记录主窗口句柄；关闭行为统一由 Slint 的 on_close_requested 处理。
pub fn attach_main_window(hwnd: HWND) {
    unsafe {
        MAIN_HWND.set(hwnd);
        if hwnd.is_null() { return; }
        let hinstance = GetModuleHandleW(std::ptr::null());
        let small = LoadImageW(
            hinstance,
            APP_ICON_ID as *const u16,
            IMAGE_ICON,
            16,
            16,
            LR_SHARED,
        );
        let big = LoadImageW(
            hinstance,
            APP_ICON_ID as *const u16,
            IMAGE_ICON,
            32,
            32,
            LR_SHARED,
        );
        if !small.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL, small as LPARAM);
        }
        if !big.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG, big as LPARAM);
        }
    }
}

/// 只有托盘已就绪才允许隐藏；窗口可见性必须由 Slint 管理。
pub fn is_ready() -> bool {
    ICON_ADDED.load(Ordering::SeqCst)
}
/// 取出所有待处理的托盘命令（界面定时器里调用）。
pub fn drain_commands() -> Vec<TrayCommand> {
    match COMMANDS.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => Vec::new(),
    }
}

/// Reuse the same restoration path for a second application launch.
pub fn request_show() {
    queue_push(TrayCommand::Show);
}

/// Let the running instance take focus on behalf of this newly launched process.
pub fn allow_existing_instance_foreground() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, FindWindowExW, GetWindowThreadProcessId,
    };
    unsafe {
        let hwnd = FindWindowExW(std::ptr::null_mut(), std::ptr::null_mut(), CLASS_NAME.as_ptr(), std::ptr::null());
        if !hwnd.is_null() {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != 0 { AllowSetForegroundWindow(pid); }
        }
    }
}


/// 显示并前置主窗口。
pub fn show_main_window() {
    unsafe {
        let h = MAIN_HWND.get();
        if h.is_null() {
            return;
        }
        // main.rs 已通知 Slint 恢复显示；这里补上 Win32 的显示和置前。
        use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos;
        const FLAGS: u32 = 0x0001 | 0x0002; // NOSIZE|NOMOVE: Slint owns visibility.
        SetWindowPos(h, std::ptr::null_mut(), 0, 0, 0, 0, FLAGS);
        SetForegroundWindow(h);
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

/// 托盘窗口过程：恢复任务栏图标，并将用户命令入队。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayWindowState;
    if !state.is_null() && msg == (*state).taskbar_created {
        // Explorer discards notification icons on restart (and can recreate the
        // taskbar after a DPI change). The previous successful Add is stale.
        ICON_ADDED.store(false, Ordering::SeqCst);
        match ((*state).add_icon)(hwnd, &(*state).tooltip) {
            Ok(()) => ICON_ADDED.store(true, Ordering::SeqCst),
            Err(error) => {
                eprintln!("[托盘] 任务栏重建后恢复图标失败：{error}");
                // Restore a hidden application so it stays reachable even when
                // the shell rejects the replacement icon.
                queue_push(TrayCommand::Show);
            }
        }
        return 0;
    }
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
            // This helper window can be destroyed when tray initialization
            // fails. The application owns the UI loop and exits explicitly.
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
    if !MAIN_HWND.get().is_null() {
        PostMessageW(MAIN_HWND.get(), 0, 0, 0);
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

#[cfg(test)]
mod tests {
    use super::*;

    static RESTORE_EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static RESTORE_TEST: Mutex<()> = Mutex::new(());

    fn record_icon_restore(_hwnd: HWND, tooltip: &str) -> Result<(), String> {
        RESTORE_EVENTS.lock().unwrap().push(tooltip.to_owned());
        Ok(())
    }

    fn reject_icon_restore(_hwnd: HWND, _tooltip: &str) -> Result<(), String> {
        Err("isolated shell failure".into())
    }

    #[test]
    fn tray_window_is_a_broadcast_target_and_readds_the_icon_after_taskbar_recreation() {
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        use windows_sys::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, FindWindowExW, IsWindowVisible};
        let _guard = RESTORE_TEST.lock().unwrap();
        RESTORE_EVENTS.lock().unwrap().clear();
        unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
            (*(data as *mut Vec<HWND>)).push(hwnd);
            1
        }
        unsafe {
            let message: Vec<u16> = "TaskbarCreated".encode_utf16().chain([0]).collect();
            let taskbar_created = RegisterWindowMessageW(message.as_ptr());
            assert_ne!(taskbar_created, 0);
            let mut state = Box::new(TrayWindowState {
                tooltip: "isolated test".into(), taskbar_created, add_icon: record_icon_restore,
            });
            let hwnd = create_tray_window(&mut state).unwrap();
            let mut windows: Vec<HWND> = Vec::new();
            assert_ne!(EnumThreadWindows(GetCurrentThreadId(), Some(collect), &mut windows as *mut _ as LPARAM), 0);
            let visible = IsWindowVisible(hwnd) != 0;
            let discoverable = FindWindowExW(std::ptr::null_mut(), std::ptr::null_mut(), CLASS_NAME.as_ptr(), std::ptr::null()) == hwnd;
            // Only this private window receives the simulated restart. The icon
            // callback is a test double; Explorer and the real tray are untouched.
            SendMessageW(hwnd, taskbar_created, 0, 0);
            let restored = RESTORE_EVENTS.lock().unwrap().clone();
            let ready = is_ready();
            assert_ne!(DestroyWindow(hwnd), 0);
            ICON_ADDED.store(false, Ordering::SeqCst);
            assert!(!visible, "托盘辅助窗口不能显示在桌面");
            assert!(discoverable, "第二实例仍需找到托盘所属进程来授权前置");
            assert!(windows.contains(&hwnd), "消息专用窗口无法接收 TaskbarCreated 顶层广播");
            assert_eq!(restored, ["isolated test"], "任务栏重建后应以原提示文字重新添加图标");
            assert!(ready);
        }
    }

    #[test]
    fn failed_icon_recovery_shows_the_application_and_disables_tray_hiding() {
        let _guard = RESTORE_TEST.lock().unwrap();
        drain_commands();
        unsafe {
            let message: Vec<u16> = "TaskbarCreated".encode_utf16().chain([0]).collect();
            let taskbar_created = RegisterWindowMessageW(message.as_ptr());
            assert_ne!(taskbar_created, 0);
            let mut state = Box::new(TrayWindowState {
                tooltip: "isolated test".into(), taskbar_created, add_icon: reject_icon_restore,
            });
            let hwnd = create_tray_window(&mut state).unwrap();
            // Test state only: no icon has been installed in the actual shell.
            ICON_ADDED.store(true, Ordering::SeqCst);
            SendMessageW(hwnd, taskbar_created, 0, 0);
            let ready = is_ready();
            let commands = drain_commands();
            assert_ne!(DestroyWindow(hwnd), 0);
            ICON_ADDED.store(false, Ordering::SeqCst);
            assert!(!ready, "旧的图标注册成功状态不能沿用");
            assert_eq!(commands, [TrayCommand::Show], "恢复失败时应让已隐藏的主窗口重新可达");
        }
    }

    #[test]
    fn destroying_a_tray_message_window_does_not_quit_the_ui_thread() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            PeekMessageW, UnregisterClassW, MSG, PM_REMOVE, WM_QUIT,
        };

        // A private message-only window exercises the real destruction callback
        // without adding an icon to the user's notification area.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let class: Vec<u16> = format!("MiacTrayDestroyTest-{}-{unique}", std::process::id())
                .encode_utf16().chain([0]).collect();
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wnd_proc);
            wc.hInstance = instance;
            wc.lpszClassName = class.as_ptr();
            assert_ne!(RegisterClassW(&wc), 0);
            let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0,
                0, 0, 0, 0, -3isize as HWND, std::ptr::null_mut(), instance, std::ptr::null());
            assert!(!hwnd.is_null());
            assert_ne!(DestroyWindow(hwnd), 0);
            let mut message: MSG = std::mem::zeroed();
            let quit_posted = PeekMessageW(&mut message, std::ptr::null_mut(), WM_QUIT, WM_QUIT, PM_REMOVE) != 0;
            assert_ne!(UnregisterClassW(class.as_ptr(), instance), 0);
            assert!(!quit_posted, "托盘初始化失败后的资源清理不能结束主界面消息循环");
        }
    }
}
