//! credentials.rs —— 凭据文件的定位、读取与安全写入
//!
//! 移植自 v1 `lib/credentials.js`，行为逐条对齐，保证 v1 → v2 无缝复用已有凭据：
//!
//! - 三个文件：`device.json`（did/token/localip）、`cloud-session.json`、
//!   `thermometer.json`
//! - 写入目录：打包版指向 `%APPDATA%\米家空调\`
//! - 主目录没有文件时才回退旧目录，兼容旧版凭据
//! - 三个敏感文件的所有新写入都使用当前 Windows 用户范围的 DPAPI
//! - 写密文时用 icacls 收紧权限
//!
//! ## 与 v1 的差别：DPAPI
//!
//! 读取兼容 v1 明文和旧 DPAPI 密文；写入一律使用新版带文件名用途隔离的
//! DPAPI 格式。用户级保护并不能抵御已经以同一 Windows 用户身份运行的恶意程序。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cloud::CloudSession;

fn wipe_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn is_json_object(bytes: &[u8]) -> bool {
    bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'{')
        && serde_json::from_slice::<serde::de::IgnoredAny>(bytes).is_ok()
}

/// 凭据文件名（装敏感信息，写入时强制使用 DPAPI）。
pub const FILE_DEVICE: &str = "device.json";
pub const FILE_SESSION: &str = "cloud-session.json";
pub const FILE_THERMOMETER: &str = "thermometer.json";
/// 应用设置（非敏感：主题、预设温度、通道）。与凭据同目录，便于一起迁移。
pub const FILE_SETTINGS: &str = "settings.json";

/// 三个敏感凭据文件（DPAPI 加密与 icacls 收紧权限只针对它们）。
pub const CREDENTIAL_FILES: [&str; 3] = [FILE_DEVICE, FILE_SESSION, FILE_THERMOMETER];

/// 全部允许写入的文件名。
///
/// 这是个白名单：`write_bytes` 拒绝名单外的名字，避免调用方拼错或
/// 传入用户可控的字符串时把文件写到别处。
pub const ALL_FILES: [&str; 4] = [
    FILE_DEVICE,
    FILE_SESSION,
    FILE_THERMOMETER,
    FILE_SETTINGS,
];

/// 实际文件扫描结果，不依赖 settings.json 中的旧加密开关。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProtectionStatus {
    pub present: usize,
    pub current_format: usize,
    pub old_dpapi: usize,
    pub plaintext: usize,
    pub unreadable: usize,
    /// 回退目录里的旧明文副本；不会在不知情时删除或改写。
    pub legacy_plaintext: usize,
}

#[derive(Debug, Default)]
pub struct UpgradeReport {
    pub upgraded: usize,
    pub failures: Vec<String>,
}

impl ProtectionStatus {
    pub fn active_files_protected(self) -> bool {
        self.present > 0 && self.current_format == self.present
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileProtection {
    Plaintext,
    Current,
    OldDpapi,
    Unreadable,
}

fn classify_file(file: &str, path: &Path) -> FileProtection {
    let Ok(mut raw) = fs::read(path) else { return FileProtection::Unreadable };
    if is_json_object(&raw) {
        wipe_bytes(&mut raw);
        return FileProtection::Plaintext;
    }
    let decrypted = crate::dpapi::unprotect_for(file, &raw).map(|mut plain| {
        let valid = is_json_object(&plain);
        wipe_bytes(&mut plain);
        valid
    }).unwrap_or(false);
    let current = crate::dpapi::is_current_format(&raw);
    wipe_bytes(&mut raw);
    if decrypted {
        if current {
            FileProtection::Current
        } else {
            FileProtection::OldDpapi
        }
    } else {
        FileProtection::Unreadable
    }
}

/// `device.json` —— 空调的 did / token / localip。
///
/// 字段名与 v1 完全一致；用 `#[serde(default)]` 容忍缺字段，
/// 这样 v1 写的文件里多余或缺少的键都不会导致解析失败。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct DeviceInfo {
    /// 设备名（界面显示用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 机型标识，用于校验是否为目标机型
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 设备 did（云端与局域网 RPC 都要）
    #[serde(default)]
    pub did: String,
    /// 局域网 token（32 位 hex）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// 局域网 IP
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub localip: Option<String>,
    /// 写入时间
    #[serde(rename = "savedAt", default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl DeviceInfo {
    /// 是否具备局域网直连条件（缺一不可）。
    pub fn local_ready(&self) -> bool {
        self.localip.as_deref().is_some_and(|s| !s.trim().is_empty())
            && self.token.as_deref().is_some_and(|s| !s.trim().is_empty())
    }

    /// 机型是否匹配目标机型（没写 model 时按「未知但可用」处理，与 v1 一致）。
    pub fn model_matches(&self, expect: &str) -> bool {
        match self.model.as_deref() {
            None => true,
            Some(m) if m.trim().is_empty() => true,
            Some(m) => m == expect,
        }
    }
}

/// `thermometer.json` —— 温湿度计的型号与 did。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ThermometerInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub did: String,
    #[serde(rename = "savedAt", default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

/// 凭据存储：负责目录选择、回退查找与写入。
#[derive(Debug, Clone)]
pub struct Credentials {
    /// 主目录（新写入的凭据落这里）
    primary: PathBuf,
    /// 回退目录（旧位置，只读）
    fallbacks: Vec<PathBuf>,
}

impl Credentials {
    /// 开发期 / 便携版：主目录 = 可执行文件所在目录。
    pub fn portable() -> Self {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        Self { primary: dir, fallbacks: Vec::new() }
    }

    /// 新建：指定主目录，并把默认位置加为回退。
    pub fn new(primary: impl Into<PathBuf>) -> Self {
        let primary = primary.into();
        let mut fallbacks = Vec::new();

        // 回退 1：%APPDATA%\米家空调\（v1 安装版/便携版用的位置）
        if let Some(appdata) = user_data_dir() {
            if appdata != primary {
                fallbacks.push(appdata);
            }
        }
        // 回退 2：可执行文件所在目录（便携版把凭据放在程序旁边）
        if let Some(exe_dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            if exe_dir != primary && !fallbacks.contains(&exe_dir) {
                fallbacks.push(exe_dir);
            }
        }
        // 回退 3：当前工作目录（开发模式）
        if let Ok(cwd) = std::env::current_dir() {
            if cwd != primary && !fallbacks.contains(&cwd) {
                fallbacks.push(cwd);
            }
        }

        Self { primary, fallbacks }
    }

    /// v2 图形版应该用这个：主目录 = `%APPDATA%\米家空调\`，与 v1 完全一致，
    /// 这样旧版留下的凭据会被直接复用。
    pub fn appdata() -> Self {
        match user_data_dir() {
            Some(d) => Self::new(d),
            None => Self::portable(),
        }
    }

    /// 测试用：只认这一个目录，**不挂任何回退目录**。
    ///
    /// 为什么必须有这个构造器：`new()` 会把 `%APPDATA%\米家空调\`、可执行文件
    /// 目录、当前工作目录都加成回退。写测试时如果用它，测试就可能静默读到
    /// **本机真实的凭据**——测试结果随开发机状态变化，非常隐蔽（本项目第一版
    /// worker 测试就踩了这个坑：临时目录是空的，却从回退目录读到了真凭据，
    /// 于是「应该初始化失败」的断言反而不成立）。
    /// 需要确定性的场景一律用这个。
    pub fn isolated(only: impl Into<PathBuf>) -> Self {
        Self { primary: only.into(), fallbacks: Vec::new() }
    }

    pub fn primary_dir(&self) -> &Path {
        &self.primary
    }

    /// 依次尝试的目录：主目录优先，然后回退目录。
    pub fn search_dirs(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.primary.as_path()).chain(self.fallbacks.iter().map(|p| p.as_path()))
    }

    /// 实际读到该文件的位置（界面提示用），找不到返回 None。
    pub fn locate(&self, file: &str) -> Option<PathBuf> {
        self.search_dirs()
            .map(|d| d.join(file))
            .find(|p| p.is_file())
    }

    pub fn exists(&self, file: &str) -> bool {
        self.locate(file).is_some()
    }

    /// 只检查凭据文件本身，不把历史设置开关当作加密成功的证明。
    pub fn protection_status(&self) -> ProtectionStatus {
        let mut status = ProtectionStatus::default();
        for file in CREDENTIAL_FILES {
            if let Some(path) = self.locate(file) {
                status.present += 1;
                match classify_file(file, &path) {
                    FileProtection::Plaintext => status.plaintext += 1,
                    FileProtection::Current => status.current_format += 1,
                    FileProtection::OldDpapi => status.old_dpapi += 1,
                    FileProtection::Unreadable => status.unreadable += 1,
                }
            }
            for dir in &self.fallbacks {
                let path = dir.join(file);
                if path.is_file() && classify_file(file, &path) == FileProtection::Plaintext {
                    status.legacy_plaintext += 1;
                }
            }
        }
        status
    }

    /// 把所有可读的旧明文/旧 DPAPI 凭据升级到当前格式，供启动和按钮复用。
    /// 每个文件单独原子写入；损坏文件不会被覆盖，旧位置副本不会被删。
    pub fn upgrade_existing(&self) -> UpgradeReport {
        let mut report = UpgradeReport::default();
        for file in CREDENTIAL_FILES {
            let Some(path) = self.locate(file) else { continue };
            if classify_file(file, &path) == FileProtection::Current {
                continue;
            }
            let Some(value) = self.read_json::<serde_json::Value>(file) else {
                report.failures.push(format!("{file} 无法读取或解密"));
                continue;
            };
            if !value.is_object() {
                report.failures.push(format!("{file} 不是有效的凭据对象"));
                continue;
            }
            match self.write_json(file, &value) {
                Ok(written) if classify_file(file, &written) == FileProtection::Current => {
                    report.upgraded += 1;
                }
                Ok(_) => report.failures.push(format!("{file} 写入后校验失败")),
                Err(e) => report.failures.push(format!("{file} 加密写入失败：{e}")),
            }
        }
        report
    }

    /// 读取并解析为 JSON；找不到或坏掉都返回 None（调用方按「未配置」处理）。
    pub fn read_json<T: serde::de::DeserializeOwned>(&self, file: &str) -> Option<T> {
        // 主目录的文件若存在却损坏或无法解密，不退回到陈旧的旧版副本。
        let path = self.locate(file)?;
        let mut raw = fs::read(path).ok()?;
        if let Ok(value) = serde_json::from_slice(&raw) {
            if CREDENTIAL_FILES.contains(&file) { wipe_bytes(&mut raw); }
            return Some(value);
        }
        let decrypted = crate::dpapi::unprotect_for(file, &raw);
        wipe_bytes(&mut raw);
        let mut plain = decrypted?;
        let value = serde_json::from_slice(&plain).ok();
        wipe_bytes(&mut plain);
        value
    }

    pub fn read_device(&self) -> Option<DeviceInfo> {
        self.read_json(FILE_DEVICE)
    }

    pub fn read_session(&self) -> Option<CloudSession> {
        self.read_json(FILE_SESSION)
    }

    pub fn read_thermometer(&self) -> Option<ThermometerInfo> {
        self.read_json(FILE_THERMOMETER)
    }

    /// 写 JSON：敏感文件强制 DPAPI；仅 settings.json 保持明文。
    pub fn write_json<T: serde::Serialize>(&self, file: &str, data: &T) -> std::io::Result<PathBuf> {
        if !ALL_FILES.contains(&file) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "未知的凭据文件"));
        }
        let mut text = serde_json::to_vec_pretty(data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if file == FILE_SETTINGS {
            return self.write_bytes(file, &text);
        }
        let protected = crate::dpapi::protect_for(file, &text);
        wipe_bytes(&mut text);
        let blob = protected
            .map_err(|e| std::io::Error::other(format!("DPAPI 加密失败：{e}")))?;
        self.write_bytes(file, &blob)
    }

    /// 底层写入：建目录 → 写文件 → 收紧权限。
    fn write_bytes(&self, file: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        if !ALL_FILES.contains(&file) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("未知的凭据文件 \"{file}\""),
            ));
        }

        fs::create_dir_all(&self.primary)?;
        let path = self.primary.join(file);
        let temp = self.primary.join(format!(
            ".{file}.{}.{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let write_result = (|| -> std::io::Result<()> {
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            out.write_all(bytes)?;
            out.sync_all()?;

            // 先收紧临时文件；失败时旧文件不受影响，临时文件会被清理。
            #[cfg(windows)]
            harden_permissions(&temp)?;
            fs::rename(&temp, &path)?;
            Ok(())
        })();
        if let Err(e) = write_result {
            let _ = fs::remove_file(&temp);
            return Err(e);
        }

        Ok(path)
    }
}

/// `%APPDATA%\米家空调\` —— 与 v1 的 `app.getPath('userData')` 同名同路径。
pub fn user_data_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("米家空调"))
}

/// 用进程令牌里的真实 SID，而不是可伪造的 USERNAME 环境变量。
#[cfg(windows)]
fn current_user_sid() -> std::io::Result<String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            let mut size = 0u32;
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
            if size < std::mem::size_of::<TOKEN_USER>() as u32 {
                return Err(std::io::Error::last_os_error());
            }
            let words = (size as usize).div_ceil(std::mem::size_of::<usize>());
            let mut buffer = vec![0usize; words];
            if GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            ) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let mut sid_text = ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid_text) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut len = 0usize;
            while len < 256 && *sid_text.add(len) != 0 {
                len += 1;
            }
            let converted = if len == 256 {
                Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Windows SID 过长"))
            } else {
                String::from_utf16(std::slice::from_raw_parts(sid_text, len))
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            };
            LocalFree(sid_text.cast());
            converted
        })();
        CloseHandle(token);
        result
    }
}

/// 用 icacls 断掉权限继承，只保留当前用户可访问。
///
/// 与 v1 `credentials.js` 的做法一致：失败直接报错，绝不留下权限过宽的凭据文件。
#[cfg(windows)]
pub(crate) fn harden_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    let sid = current_user_sid()?;

    let out = Command::new("icacls")
        // A GUI parent does not hide console children automatically. Settings
        // saves also use this path, so never create a console for the ACL tool.
        .creation_flags(CREATE_NO_WINDOW)
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("*{sid}:(F)"))
        .output()?;

    if !out.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "无法收紧凭据文件权限：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("miac-credentials-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn device_json_reads_v1_format() {
        // v1 写出来的 device.json 原样可读
        let json = r#"{
            "name": "巨省电 大2匹",
            "model": "xiaomi.airc.h53h00",
            "did": "123456789",
            "token": "00112233445566778899aabbccddeeff",
            "localip": "192.168.1.50",
            "savedAt": "2026-09-01T10:00:00.000Z"
        }"#;
        let d: DeviceInfo = serde_json::from_str(json).unwrap();
        assert_eq!(d.did, "123456789");
        assert!(d.local_ready());
        assert!(d.model_matches("xiaomi.airc.h53h00"));
        assert!(!d.model_matches("xiaomi.aircondition.mh4"));
    }

    #[test]
    fn device_without_localip_is_not_local_ready() {
        let json = r#"{ "did": "1", "token": "aa" }"#;
        let d: DeviceInfo = serde_json::from_str(json).unwrap();
        assert!(!d.local_ready(), "缺 localip 时不能走局域网");
    }

    #[test]
    fn device_roundtrip_keeps_v1_field_names() {
        let d = DeviceInfo {
            name: Some("空调".into()),
            model: Some("xiaomi.airc.h53h00".into()),
            did: "42".into(),
            token: Some("ff".into()),
            localip: Some("10.0.0.2".into()),
            saved_at: Some("2026-09-19T00:00:00.000Z".into()),
        };
        let s = serde_json::to_string(&d).unwrap();
        // v1 用的是 savedAt（驼峰），必须保持一致
        assert!(s.contains("\"savedAt\""));
        assert!(s.contains("\"localip\""));
    }

    #[test]
    fn user_data_dir_matches_v1() {
        // 只验证拼出来的最后一段目录名与 v1 一致
        if let Some(d) = user_data_dir() {
            assert_eq!(d.file_name().unwrap(), "米家空调");
        }
    }

    #[test]
    fn rejects_unknown_file_names() {
        let c = Credentials::portable();
        let err = c.write_bytes("evil.json", b"{}").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[cfg(windows)]
    #[test]
    fn all_sensitive_writes_are_encrypted_but_settings_stay_plaintext() {
        let dir = test_dir("write");
        let creds = Credentials::isolated(&dir);
        let value = serde_json::json!({ "did": "test-only", "token": "test-secret" });
        for file in CREDENTIAL_FILES {
            creds.write_json(file, &value).unwrap();
            let raw = fs::read(dir.join(file)).unwrap();
            assert!(crate::dpapi::is_current_format(&raw));
            assert!(!String::from_utf8_lossy(&raw).contains("test-secret"));
            assert_eq!(creds.read_json::<serde_json::Value>(file).unwrap(), value);
            // 重复写入必须仍可替换目标文件，不能因 Windows rename 语义失败。
            creds.write_json(file, &value).unwrap();
        }
        creds.write_json(FILE_SETTINGS, &serde_json::json!({"dark": true})).unwrap();
        assert!(fs::read_to_string(dir.join(FILE_SETTINGS)).unwrap().contains("dark"));
        assert!(creds.protection_status().active_files_protected());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn upgrades_legacy_plaintext_without_deleting_old_copy() {
        let root = test_dir("upgrade");
        let primary = root.join("new");
        let old = root.join("old");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join(FILE_DEVICE), br#"{"did":"legacy","token":"test-secret"}"#).unwrap();
        let creds = Credentials { primary: primary.clone(), fallbacks: vec![old.clone()] };
        assert_eq!(creds.protection_status().plaintext, 1);
        let report = creds.upgrade_existing();
        assert_eq!(report.upgraded, 1);
        assert!(report.failures.is_empty());
        assert!(crate::dpapi::is_current_format(&fs::read(primary.join(FILE_DEVICE)).unwrap()));
        assert_eq!(creds.read_device().unwrap().did, "legacy");
        assert_eq!(creds.protection_status().legacy_plaintext, 1);
        assert!(old.join(FILE_DEVICE).exists(), "旧版数据不能被暗中删除");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn corrupt_primary_never_falls_back_to_stale_plaintext() {
        let root = test_dir("fail-closed");
        let primary = root.join("new");
        let old = root.join("old");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::write(primary.join(FILE_DEVICE), b"damaged ciphertext").unwrap();
        fs::write(old.join(FILE_DEVICE), br#"{"did":"stale"}"#).unwrap();
        let creds = Credentials { primary, fallbacks: vec![old] };
        assert!(creds.read_device().is_none());
        let report = creds.upgrade_existing();
        assert_eq!(report.upgraded, 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(creds.protection_status().unreadable, 1);
        fs::remove_dir_all(root).unwrap();
    }
}
