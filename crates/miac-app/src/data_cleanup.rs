//! Delete only Miac-owned data, after the desktop process and its writers exit.
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
use miac_core::credentials::FILE_SETTINGS;

const APP_DIR: &str = "米家空调";
const REGISTRY_KEYS: [&str; 1] = [r"Software\米家空调"];

#[cfg(test)]
thread_local! {
    static AFTER_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce(&Path)>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Default, Debug)]
struct CleanupReport {
    files: usize,
    directories: usize,
    registry_keys: usize,
    failures: Vec<String>,
}

impl CleanupReport {
    fn record(&mut self, path: &Path, error: impl std::fmt::Display) {
        self.failures.push(format!("{}：{error}", path.display()));
    }

    fn message(&self) -> String {
        let counts = format!("已删除 {} 个文件、{} 个缓存目录和 {} 个注册表项。", self.files, self.directories, self.registry_keys);
        if self.failures.is_empty() {
            format!("凭证及本机数据已清理完成。\n{counts}\n\n重新打开程序后，请重新扫码登录。")
        } else {
            format!("部分数据未能删除。\n{counts}\n\n{}\n\n请关闭占用文件的旧版程序后重试。", self.failures.join("\n"))
        }
    }
}

struct CleanupPlan {
    data_dirs: Vec<PathBuf>,
    executable: PathBuf,
}

impl CleanupPlan {
    fn current() -> std::io::Result<Self> {
        let executable = std::env::current_exe()?;
        let data_dirs: Vec<_> = ["APPDATA", "LOCALAPPDATA"].iter()
            .filter_map(|key| std::env::var_os(key).map(|base| PathBuf::from(base).join(APP_DIR)))
            .collect();
        Ok(Self { data_dirs, executable })
    }

    fn execute_files(&self) -> CleanupReport {
        let mut report = CleanupReport::default();
        let resolved_executable = fs::canonicalize(&self.executable).ok();
        for directory in &self.data_dirs {
            // Never recursively delete a root, arbitrary folder, or a directory
            // containing the running binary, even with malformed environment vars.
            if !directory.is_absolute() || directory.file_name().is_none_or(|name| name != APP_DIR)
                || directory.parent().and_then(Path::file_name).is_none()
                || self.executable.starts_with(directory)
                || resolved_executable.as_ref().zip(fs::canonicalize(directory).ok())
                    .is_some_and(|(executable, directory)| executable.starts_with(directory))
            {
                report.record(directory, "数据目录不符合清理范围，已保留");
                continue;
            }
            remove_tree(directory, true, &mut report);
        }
        report
    }
}

fn reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    { metadata.file_type().is_symlink() }
}

#[cfg(not(windows))]
fn remove_tree(path: &Path, root: bool, report: &mut CleanupReport) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => { report.record(path, error); return; }
    };
    if root && metadata.is_dir() && !reparse_point(&metadata) {
        // Verify the resolved target is the named child of the intended parent.
        let resolved = fs::canonicalize(path);
        let expected = path.parent().and_then(|parent| fs::canonicalize(parent).ok())
            .zip(path.file_name()).map(|(parent, name)| parent.join(name));
        if !matches!((resolved, expected), (Ok(resolved), Some(expected)) if resolved == expected) {
            report.record(path, "无法验证清理目录的实际位置，已保留");
            return;
        }
    }
    if reparse_point(&metadata) {
        if root {
            report.record(path, "清理根目录是链接或联接，已保留");
            return;
        }
        // Unlink the entry itself; do not follow a link outside the application.
        #[cfg(windows)]
        let is_dir = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x10 != 0
        };
        #[cfg(not(windows))]
        let is_dir = false;
        let result = if is_dir { fs::remove_dir(path) } else { fs::remove_file(path) };
        match result {
            Ok(()) => report.files += 1,
            Err(error) => report.record(path, error),
        }
    } else if metadata.is_dir() {
        match fs::read_dir(path) {
            Ok(entries) => for entry in entries {
                match entry {
                    Ok(entry) => remove_tree(&entry.path(), false, report),
                    Err(error) => report.record(path, error),
                }
            },
            Err(error) => { report.record(path, error); return; }
        }
        match fs::remove_dir(path) {
            Ok(()) => report.directories += 1,
            Err(error) => report.record(path, error),
        }
    } else {
        // Read-only cache files must not leave account data behind.
        if metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            if let Err(error) = fs::set_permissions(path, permissions) {
                report.record(path, error);
                return;
            }
        }
        match fs::remove_file(path) {
            Ok(()) => report.files += 1,
            Err(error) => report.record(path, error),
        }
    }
}

/// Keep every traversed directory open without FILE_SHARE_DELETE. A rename or
/// junction replacement must fail while its children are being enumerated.
/// Open each leaf with OPEN_REPARSE_POINT, inspect that handle, then delete that
/// same object by handle rather than resolving the path a second time.
#[cfg(windows)]
fn remove_tree(path: &Path, root: bool, report: &mut CleanupReport) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        SetFileInformationByHandle, FileDispositionInfo, FILE_DISPOSITION_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    fn open(path: &Path, delete: bool) -> std::io::Result<fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new().read(true)
            .access_mode(0x80000000 | if delete { 0x10000 | 0x100 } else { 0 })
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    let result = (|| -> std::io::Result<()> {
        let mut ancestors = Vec::new();
        if root {
            let mut paths: Vec<_> = path.parent().into_iter().flat_map(Path::ancestors).collect();
            paths.reverse();
            for parent in paths {
                let guard = open(parent, false)?;
                if reparse_point(&guard.metadata()?) {
                    return Err(std::io::Error::other("清理目录的上级是链接或联接，已保留"));
                }
                ancestors.push(guard);
            }
        }
        let file = match open(path, true) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let metadata = file.metadata()?;
        #[cfg(test)]
        AFTER_OPEN.with(|slot| {
            let hook = slot.borrow_mut().take();
            if let Some(hook) = hook { hook(path); }
        });
        let linked = reparse_point(&metadata);
        if root && linked { return Err(std::io::Error::other("清理根目录是链接或联接，已保留")); }
        if metadata.is_dir() && !linked {
            for entry in fs::read_dir(path)? {
                remove_tree(&entry?.path(), false, report);
            }
        }
        if !linked && metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            file.set_permissions(permissions)?;
        }
        let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
        let ok = unsafe { SetFileInformationByHandle(file.as_raw_handle(), FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(), std::mem::size_of_val(&info) as u32) };
        if ok == 0 { return Err(std::io::Error::last_os_error()); }
        if metadata.is_dir() && !linked { report.directories += 1; } else { report.files += 1; }
        Ok(())
    })();
    if let Err(error) = result { report.record(path, error); }
}

#[cfg(windows)]
fn delete_registry_key(key: &str, view: u32) -> std::io::Result<bool> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegDeleteKeyExW, RegDeleteTreeW, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    let key: Vec<u16> = key.encode_utf16().chain([0]).collect();
    let mut handle = std::ptr::null_mut();
    unsafe {
        let result = RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_READ | KEY_WRITE | 0x10000 | view, &mut handle);
        if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND { return Ok(false); }
        if result != ERROR_SUCCESS { return Err(std::io::Error::from_raw_os_error(result as i32)); }
        let cleared = RegDeleteTreeW(handle, std::ptr::null());
        RegCloseKey(handle);
        if cleared != ERROR_SUCCESS { return Err(std::io::Error::from_raw_os_error(cleared as i32)); }
        let removed = RegDeleteKeyExW(HKEY_CURRENT_USER, key.as_ptr(), view, 0);
        if removed == ERROR_SUCCESS || removed == ERROR_FILE_NOT_FOUND { Ok(true) }
        else { Err(std::io::Error::from_raw_os_error(removed as i32)) }
    }
}

#[cfg(windows)]
pub fn launch() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(std::env::current_exe()?)
        .arg("--cleanup-data")
        .arg(std::process::id().to_string())
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
pub fn launch() -> std::io::Result<()> {
    Err(std::io::Error::other("一键清理目前仅支持 Windows"))
}

#[cfg(windows)]
fn wait_for_parent(pid: u32) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) { Ok(()) } else { Err(error) };
        }
        let waited = WaitForSingleObject(handle, 90_000);
        CloseHandle(handle);
        if waited == WAIT_OBJECT_0 { Ok(()) }
        else { Err(std::io::Error::other("程序尚未完全退出，未开始删除数据，请稍后重试")) }
    }
}

pub fn run_if_requested() -> bool {
    let mut args = std::env::args().skip(1);
    if !matches!(args.next().as_deref(), Some("--cleanup-data" | "--cleanup-data-v3")) { return false; }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{KEY_WOW64_32KEY, KEY_WOW64_64KEY};
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND};
        let result = (|| -> std::io::Result<CleanupReport> {
            let pid = args.next().and_then(|pid| pid.parse().ok()).filter(|pid| *pid != std::process::id())
                .ok_or_else(|| std::io::Error::other("缺少有效的退出进程，未执行清理"))?;
            wait_for_parent(pid)?;
            // Prevent a new desktop instance from recreating data during cleanup.
            let _instance = crate::single_instance::SingleInstance::acquire()?
                .ok_or_else(|| std::io::Error::other("米家空调又被打开了，请先退出再执行清理"))?;
            let mut report = CleanupPlan::current()?.execute_files();
            match crate::autostart::remove() {
                Ok(true) => report.registry_keys += 1,
                Ok(false) => {}
                Err(error) => report.failures.push(format!("Windows 自启动项：{error}")),
            }
            for key in REGISTRY_KEYS {
                for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
                    match delete_registry_key(key, view) {
                        Ok(true) => report.registry_keys += 1,
                        Ok(false) => {}
                        Err(error) => report.failures.push(format!("HKCU\\{key}：{error}")),
                    }
                }
            }
            Ok(report)
        })();
        let (message, icon) = match result {
            Ok(report) => (report.message(), if report.failures.is_empty() { MB_ICONINFORMATION } else { MB_ICONERROR }),
            Err(error) => (format!("清理未完成：{error}"), MB_ICONERROR),
        };
        let wide = |value: &str| value.encode_utf16().chain([0]).collect::<Vec<_>>();
        unsafe { MessageBoxW(std::ptr::null_mut(), wide(&message).as_ptr(), wide("一键删除凭证").as_ptr(), MB_OK | icon | MB_SETFOREGROUND); }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn open_directory_cannot_be_replaced_between_validation_and_deletion() {
        let root = fixture();
        let data = root.join(APP_DIR);
        let moved = root.join("replaced");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("account-cache"), b"synthetic").unwrap();
        AFTER_OPEN.with(|slot| *slot.borrow_mut() = Some(Box::new(move |path| {
            let error = fs::rename(path, &moved).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(32), "a live directory handle must deny replacement");
        })));
        let report = CleanupPlan { data_dirs: vec![data.clone()], executable: root.join("miac-app.exe") }.execute_files();
        assert!(report.failures.is_empty(), "{report:?}");
        assert!(!data.exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn fixture() -> PathBuf {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("miac-cleanup-{}-{unique}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        assert!(fs::canonicalize(&root).unwrap().starts_with(fs::canonicalize(std::env::temp_dir()).unwrap()));
        root
    }

    #[test]
    fn cleans_only_named_app_data_and_preserves_portable_files() {
        let root = fixture();
        let roaming = root.join("roaming").join(APP_DIR);
        let local = root.join("local").join(APP_DIR);
        let portable = root.join("portable");
        for directory in [&roaming, &local, &portable] { fs::create_dir_all(directory).unwrap(); }
        for directory in [&roaming, &portable] {
            for file in ["device.json", "cloud-session.json", "thermometer.json", "settings.json", "credentials.json", ".device.json.1.2.tmp", "miac-app.log"] {
                fs::write(directory.join(file), b"fixture data").unwrap();
            }
            fs::create_dir_all(directory.join("miot-spec-cache")).unwrap();
            fs::write(directory.join("miot-spec-cache/model.json"), b"{}").unwrap();
        }
        fs::create_dir_all(local.join("Local Storage/leveldb")).unwrap();
        fs::write(local.join("Local Storage/leveldb/cache.ldb"), b"old login cache").unwrap();
        for file in ["miac-app.exe", "README.md", "unrelated.log", "my-device.json"] {
            fs::write(portable.join(file), b"keep").unwrap();
        }
        let plan = CleanupPlan { data_dirs: vec![roaming.clone(), local.clone()], executable: portable.join("miac-app.exe") };
        let report = plan.execute_files();
        assert!(report.failures.is_empty(), "{report:?}");
        assert!(!roaming.exists() && !local.exists());
        assert!(portable.join("device.json").exists() && portable.join("miot-spec-cache").exists());
        for file in ["miac-app.exe", "README.md", "unrelated.log", "my-device.json"] { assert!(portable.join(file).exists(), "{file}"); }
        assert!(plan.execute_files().failures.is_empty(), "再次清理不存在的数据也应成功");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_an_unrelated_data_root_and_cleans_read_only_cache_files() {
        let root = fixture();
        let unrelated = root.join("unrelated");
        let data = root.join(APP_DIR);
        fs::create_dir_all(&unrelated).unwrap();
        fs::create_dir_all(&data).unwrap();
        fs::write(unrelated.join("keep.txt"), b"keep").unwrap();
        let cache = data.join("read-only-cache");
        fs::write(&cache, b"cache").unwrap();
        let mut permissions = fs::metadata(&cache).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&cache, permissions).unwrap();
        let plan = CleanupPlan { data_dirs: vec![unrelated.clone(), data.clone()], executable: root.join("miac-app.exe") };
        let report = plan.execute_files();
        assert_eq!(report.failures.len(), 1);
        assert!(unrelated.join("keep.txt").exists());
        assert!(!data.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn removes_registry_values_and_subkeys_only_in_an_isolated_test_key() {
        use windows_sys::Win32::System::Registry::{RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_WOW64_64KEY, REG_SZ};
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let key = format!(r"Software\MiacCleanupTest-{}-{unique}", std::process::id());
        let child: Vec<_> = format!(r"{key}\Cache").encode_utf16().chain([0]).collect();
        let mut handle = std::ptr::null_mut();
        unsafe {
            assert_eq!(RegCreateKeyExW(HKEY_CURRENT_USER, child.as_ptr(), 0, std::ptr::null(), 0, KEY_ALL_ACCESS | KEY_WOW64_64KEY, std::ptr::null(), &mut handle, std::ptr::null_mut()), 0);
            assert_eq!(RegSetValueExW(handle, std::ptr::null(), 0, REG_SZ, [0u8, 0].as_ptr(), 2), 0);
            RegCloseKey(handle);
        }
        assert!(delete_registry_key(&key, KEY_WOW64_64KEY).unwrap());
        assert!(!delete_registry_key(&key, KEY_WOW64_64KEY).unwrap(), "测试项应已彻底删除");
    }

    #[cfg(windows)]
    #[test]
    fn does_not_follow_directory_junctions_outside_the_data_directory() {
        use std::os::windows::process::CommandExt;
        let root = fixture();
        let data = root.join(APP_DIR);
        let outside = root.join("unrelated");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let link = data.join("cache-link");
        let quote = |path: &Path| path.display().to_string().replace('\'', "''");
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null", quote(&link), quote(&outside)))
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let plan = CleanupPlan { data_dirs: vec![data.clone()], executable: root.join("miac-app.exe") };
        let report = plan.execute_files();
        assert!(report.failures.is_empty(), "{report:?}");
        assert!(!data.exists());
        assert!(outside.join("keep.txt").exists(), "不能沿目录联接删除外部文件");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn does_not_follow_a_portable_root_junction_into_an_unrelated_directory() {
        use std::os::windows::process::CommandExt;
        let root = fixture();
        let outside = root.join("unrelated");
        fs::create_dir_all(&outside).unwrap();
        let settings = outside.join(FILE_SETTINGS);
        fs::write(&settings, b"keep unrelated settings").unwrap();
        let nested = outside.join("build");
        fs::create_dir_all(&nested).unwrap();
        let nested_settings = nested.join(FILE_SETTINGS);
        fs::write(&nested_settings, b"keep nested settings").unwrap();
        let link = root.join(APP_DIR);
        let quote = |path: &Path| path.display().to_string().replace('\'', "''");
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null", quote(&link), quote(&outside)))
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let plan = CleanupPlan {
            data_dirs: vec![link.clone()],
            executable: root.join("miac-app.exe"),
        };
        let report = plan.execute_files();
        let preserved = fs::read(&settings).ok();
        let nested_preserved = fs::read(&nested_settings).ok();
        fs::remove_dir(&link).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(!report.failures.is_empty(), "应报告并保留链接根目录");
        assert_eq!(preserved.as_deref(), Some(&b"keep unrelated settings"[..]),
            "便携数据扫描不能绕过清理根目录的联接保护");
        assert_eq!(nested_preserved.as_deref(), Some(&b"keep nested settings"[..]),
            "目录联接的下级路径也不能作为便携清理入口");
    }

    #[cfg(windows)]
    #[test]
    fn preserves_the_program_directory_when_executable_path_has_different_case() {
        let root = fixture();
        let data = root.join("MixedCase").join(APP_DIR);
        fs::create_dir_all(&data).unwrap();
        let executable = root.join("mixedcase").join(APP_DIR).join("miac-app.exe");
        fs::write(&executable, b"keep program").unwrap();
        fs::write(data.join(FILE_SETTINGS), b"{}").unwrap();
        let plan = CleanupPlan { data_dirs: vec![data], executable: executable.clone() };
        let report = plan.execute_files();
        let preserved = fs::read(executable).ok();
        fs::remove_dir_all(root).unwrap();
        assert!(!report.failures.is_empty(), "应保留包含程序的真实目录");
        assert_eq!(preserved.as_deref(), Some(&b"keep program"[..]),
            "Windows 大小写不同的路径仍指向同一程序目录");
    }

    #[cfg(windows)]
    #[test]
    fn waits_for_the_last_background_write_before_removing_data() {
        use std::os::windows::process::CommandExt;
        let root = fixture();
        let data = root.join(APP_DIR);
        fs::create_dir_all(&data).unwrap();
        let settings = data.join(FILE_SETTINGS);
        let quoted = settings.display().to_string().replace('\'', "''");
        let mut writer = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("$ErrorActionPreference='Stop'; Start-Sleep -Milliseconds 250; Set-Content -LiteralPath '{quoted}' -Value '{{}}'"))
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .spawn().unwrap();
        wait_for_parent(writer.id()).unwrap();
        assert!(writer.wait().unwrap().success());
        assert!(settings.exists(), "清理必须等最后一次后台写入完成");
        let plan = CleanupPlan { data_dirs: vec![data.clone()], executable: root.join("miac-app.exe") };
        let report = plan.execute_files();
        assert!(report.failures.is_empty(), "{report:?}");
        assert!(!data.exists(), "完成写入后再清理，设置文件不能重新出现");
        fs::remove_dir_all(root).unwrap();
    }
}
