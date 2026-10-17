//! Per-user Windows startup registration. Missing entries mean disabled;
//! registration is written only in response to an explicit settings change.
use std::path::Path;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Miac.Desktop.v2";

fn command_for(executable: &Path) -> std::io::Result<String> {
    if !executable.is_absolute() || executable.to_string_lossy().contains('"') {
        return Err(std::io::Error::other("程序路径无效，无法设置自启动"));
    }
    let command = format!("\"{}\" --autostart", executable.display());
    if command.encode_utf16().count() > 260 {
        return Err(std::io::Error::other("程序路径过长，请将程序移到较短的路径后再设置自启动"));
    }
    Ok(command)
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
        KEY_WOW64_64KEY, REG_SZ,
    };

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) { unsafe { RegCloseKey(self.0); } }
    }
    fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain([0]).collect() }
    fn result(code: u32) -> std::io::Result<()> {
        if code == ERROR_SUCCESS { Ok(()) }
        else { Err(std::io::Error::from_raw_os_error(code as i32)) }
    }
    fn open(path: &str, writable: bool) -> std::io::Result<Option<Key>> {
        let mut key = std::ptr::null_mut();
        let access = KEY_QUERY_VALUE | KEY_WOW64_64KEY | if writable { KEY_SET_VALUE } else { 0 };
        let code = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(path).as_ptr(), 0, access, &mut key) };
        if code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND { return Ok(None); }
        result(code)?;
        Ok(Some(Key(key)))
    }
    pub(super) fn read(path: &str, name: &str) -> std::io::Result<Option<String>> {
        let Some(key) = open(path, false)? else { return Ok(None) };
        let mut size = 0;
        let mut kind = 0;
        let name = wide(name);
        let code = unsafe { RegQueryValueExW(key.0, name.as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut size) };
        if code == ERROR_FILE_NOT_FOUND { return Ok(None); }
        result(code)?;
        if kind != REG_SZ || size > 8192 || size % 2 != 0 {
            return Err(std::io::Error::other("Windows 自启动项格式无效"));
        }
        let mut data = vec![0u16; size as usize / 2 + 1];
        unsafe { result(RegQueryValueExW(key.0, name.as_ptr(), std::ptr::null(), &mut kind, data.as_mut_ptr().cast(), &mut size))?; }
        let end = data.iter().position(|value| *value == 0).unwrap_or(data.len());
        String::from_utf16(&data[..end]).map(Some).map_err(|_| std::io::Error::other("Windows 自启动项编码无效"))
    }
    pub(super) fn write(path: &str, name: &str, command: &str) -> std::io::Result<()> {
        let mut key = std::ptr::null_mut();
        unsafe { result(RegCreateKeyExW(HKEY_CURRENT_USER, wide(path).as_ptr(), 0, std::ptr::null(), 0,
            KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY, std::ptr::null(), &mut key, std::ptr::null_mut()))?; }
        let key = Key(key);
        let data = wide(command);
        unsafe { result(RegSetValueExW(key.0, wide(name).as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32))?; }
        if read(path, name)?.as_deref() != Some(command) {
            return Err(std::io::Error::other("Windows 自启动项保存后校验失败"));
        }
        Ok(())
    }
    pub(super) fn remove(path: &str, name: &str) -> std::io::Result<bool> {
        let Some(key) = open(path, true)? else { return Ok(false) };
        let code = unsafe { RegDeleteValueW(key.0, wide(name).as_ptr()) };
        if code == ERROR_FILE_NOT_FOUND { return Ok(false); }
        result(code)?;
        if read(path, name)?.is_some() {
            return Err(std::io::Error::other("Windows 自启动项未能移除"));
        }
        Ok(true)
    }
}

pub fn enabled() -> std::io::Result<bool> {
    #[cfg(windows)]
    { Ok(registry::read(RUN_KEY, VALUE_NAME)?.as_deref() == Some(command_for(&std::env::current_exe()?)?.as_str())) }
    #[cfg(not(windows))]
    { Ok(false) }
}

pub fn remove() -> std::io::Result<bool> {
    #[cfg(windows)]
    { registry::remove(RUN_KEY, VALUE_NAME) }
    #[cfg(not(windows))]
    { Ok(false) }
}

pub fn set_enabled(on: bool) -> std::io::Result<bool> {
    #[cfg(windows)]
    {
        if on { registry::write(RUN_KEY, VALUE_NAME, &command_for(&std::env::current_exe()?)?)?; }
        else { remove()?; }
        let actual = enabled()?;
        if actual != on { return Err(std::io::Error::other("Windows 自启动状态校验失败")); }
        Ok(actual)
    }
    #[cfg(not(windows))]
    { let _ = on; Err(std::io::Error::other("开机自启动目前仅支持 Windows")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn startup_command_quotes_unicode_paths_and_limits_length() {
        assert_eq!(command_for(Path::new(r"D:\工具\米家 空调\miac-app.exe")).unwrap(),
            "\"D:\\工具\\米家 空调\\miac-app.exe\" --autostart");
        assert!(command_for(Path::new("miac-app.exe")).is_err());
        assert!(command_for(&std::path::PathBuf::from(format!("D:\\{}\\miac-app.exe", "x".repeat(260)))).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn startup_is_off_by_default_and_removal_preserves_other_values() {
        use windows_sys::Win32::System::Registry::{RegDeleteKeyExW, HKEY_CURRENT_USER, KEY_WOW64_64KEY};
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let key = format!(r"Software\MiacAutostartTest-{}-{unique}", std::process::id());
        // Never register a real Run entry during tests.
        assert!(registry::read(&key, VALUE_NAME).unwrap().is_none());
        let command = command_for(Path::new(r"D:\测试 程序\miac-app.exe")).unwrap();
        registry::write(&key, "OtherApp", "keep").unwrap();
        registry::write(&key, VALUE_NAME, &command).unwrap();
        assert_eq!(registry::read(&key, VALUE_NAME).unwrap().as_deref(), Some(command.as_str()));
        assert!(registry::remove(&key, VALUE_NAME).unwrap());
        assert!(!registry::remove(&key, VALUE_NAME).unwrap());
        assert_eq!(registry::read(&key, "OtherApp").unwrap().as_deref(), Some("keep"));
        registry::remove(&key, "OtherApp").unwrap();
        let wide: Vec<_> = key.encode_utf16().chain([0]).collect();
        unsafe { assert_eq!(RegDeleteKeyExW(HKEY_CURRENT_USER, wide.as_ptr(), KEY_WOW64_64KEY, 0), 0); }
    }
}
